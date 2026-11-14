use anyhow::{Context as _, anyhow};
use x11rb::connection::RequestConnection;

use crate::linux::X11ClientStatePtr;
use gpui::{
    AnyWindowHandle, Bounds, Decorations, DevicePixels, ForegroundExecutor, GpuSpecs, Modifiers,
    Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow,
    Point, PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, ScaledPixels, Scene, Size,
    Tiling, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowDecorations, WindowKind, WindowParams, WindowVisibility,
    popup::{PopupOptions, place_popup},
    px,
};
use gpui_wgpu::{CompositorGpuHint, WgpuRenderer, WgpuSurfaceConfig};

use collections::FxHashSet;
use gpui_util::{ResultExt, maybe};
use raw_window_handle as rwh;
use x11rb::{
    connection::Connection,
    cookie::{Cookie, VoidCookie},
    errors::ConnectionError,
    properties::{WmHints, WmSizeHints, WmSizeHintsSpecification},
    protocol::{
        randr::ConnectionExt as _,
        sync,
        xinput::{self, ConnectionExt as _},
        xproto::{self, ClientMessageEvent, ConnectionExt, TranslateCoordinatesReply},
    },
    wrapper::ConnectionExt as _,
    xcb_ffi::XCBConnection,
};

use std::{
    cell::RefCell, ffi::c_void, fmt::Display, num::NonZeroU32, ptr::NonNull, rc::Rc, sync::Arc,
};

use super::{X11Display, XINPUT_ALL_DEVICE_GROUPS, XINPUT_ALL_DEVICES};

x11rb::atom_manager! {
    pub XcbAtoms: AtomsCookie {
        XA_ATOM,
        XdndAware,
        XdndStatus,
        XdndEnter,
        XdndLeave,
        XdndPosition,
        XdndSelection,
        XdndDrop,
        XdndFinished,
        XdndTypeList,
        XdndActionCopy,
        TextUriList: b"text/uri-list",
        UTF8_STRING,
        TEXT,
        STRING,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        TEXT_PLAIN: b"text/plain",
        XDND_DATA,
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        WM_CHANGE_STATE,
        WM_TRANSIENT_FOR,
        _NET_WM_PID,
        _NET_WM_NAME,
        _NET_WM_ICON,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_FOCUSED,
        _NET_ACTIVE_WINDOW,
        _NET_WM_SYNC_REQUEST,
        _NET_WM_SYNC_REQUEST_COUNTER,
        _NET_WM_BYPASS_COMPOSITOR,
        _NET_WM_MOVERESIZE,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_POPUP_MENU,
        _NET_WM_STATE_MODAL,
        _NET_WM_SYNC,
        _NET_SUPPORTED,
        _MOTIF_WM_HINTS,
        _GTK_SHOW_WINDOW_MENU,
        _GTK_FRAME_EXTENTS,
        _GTK_EDGE_CONSTRAINTS,
        _NET_CLIENT_LIST_STACKING,
        WM_STATE,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WORKAREA,
        _NET_CURRENT_DESKTOP,
        // Private: a zero-length append to it is how a popup gets a server timestamp to take
        // focus with (ICCCM §2.1), since the input that opened it may have gone to another client.
        _GPUI_TIMESTAMP,
    }
}

fn query_render_extent(
    xcb: &Rc<XCBConnection>,
    x_window: xproto::Window,
) -> anyhow::Result<Size<DevicePixels>> {
    let reply = get_reply(|| "X11 GetGeometry failed.", xcb.get_geometry(x_window))?;
    Ok(Size {
        width: DevicePixels(reply.width as i32),
        height: DevicePixels(reply.height as i32),
    })
}

fn resize_edge_to_moveresize(edge: ResizeEdge) -> u32 {
    match edge {
        ResizeEdge::TopLeft => 0,
        ResizeEdge::Top => 1,
        ResizeEdge::TopRight => 2,
        ResizeEdge::Right => 3,
        ResizeEdge::BottomRight => 4,
        ResizeEdge::Bottom => 5,
        ResizeEdge::BottomLeft => 6,
        ResizeEdge::Left => 7,
    }
}

#[derive(Debug)]
struct EdgeConstraints {
    top_tiled: bool,
    #[allow(dead_code)]
    top_resizable: bool,

    right_tiled: bool,
    #[allow(dead_code)]
    right_resizable: bool,

    bottom_tiled: bool,
    #[allow(dead_code)]
    bottom_resizable: bool,

    left_tiled: bool,
    #[allow(dead_code)]
    left_resizable: bool,
}

impl EdgeConstraints {
    fn from_atom(atom: u32) -> Self {
        EdgeConstraints {
            top_tiled: (atom & (1 << 0)) != 0,
            top_resizable: (atom & (1 << 1)) != 0,
            right_tiled: (atom & (1 << 2)) != 0,
            right_resizable: (atom & (1 << 3)) != 0,
            bottom_tiled: (atom & (1 << 4)) != 0,
            bottom_resizable: (atom & (1 << 5)) != 0,
            left_tiled: (atom & (1 << 6)) != 0,
            left_resizable: (atom & (1 << 7)) != 0,
        }
    }

    fn to_tiling(&self) -> Tiling {
        Tiling {
            top: self.top_tiled,
            right: self.right_tiled,
            bottom: self.bottom_tiled,
            left: self.left_tiled,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Visual {
    id: xproto::Visualid,
    colormap: u32,
    depth: u8,
}

struct VisualSet {
    inherit: Visual,
    opaque: Option<Visual>,
    transparent: Option<Visual>,
    root: u32,
    black_pixel: u32,
}

fn find_visuals(xcb: &XCBConnection, screen_index: usize) -> VisualSet {
    let screen = &xcb.setup().roots[screen_index];
    let mut set = VisualSet {
        inherit: Visual {
            id: screen.root_visual,
            colormap: screen.default_colormap,
            depth: screen.root_depth,
        },
        opaque: None,
        transparent: None,
        root: screen.root,
        black_pixel: screen.black_pixel,
    };

    for depth_info in screen.allowed_depths.iter() {
        for visual_type in depth_info.visuals.iter() {
            let visual = Visual {
                id: visual_type.visual_id,
                colormap: 0,
                depth: depth_info.depth,
            };
            log::debug!(
                "Visual id: {}, class: {:?}, depth: {}, bits_per_value: {}, masks: 0x{:x} 0x{:x} 0x{:x}",
                visual_type.visual_id,
                visual_type.class,
                depth_info.depth,
                visual_type.bits_per_rgb_value,
                visual_type.red_mask,
                visual_type.green_mask,
                visual_type.blue_mask,
            );

            if (
                visual_type.red_mask,
                visual_type.green_mask,
                visual_type.blue_mask,
            ) != (0xFF0000, 0xFF00, 0xFF)
            {
                continue;
            }
            let color_mask = visual_type.red_mask | visual_type.green_mask | visual_type.blue_mask;
            let alpha_mask = color_mask as usize ^ ((1usize << depth_info.depth) - 1);

            if alpha_mask == 0 {
                if set.opaque.is_none() {
                    set.opaque = Some(visual);
                }
            } else {
                if set.transparent.is_none() {
                    set.transparent = Some(visual);
                }
            }
        }
    }

    set
}

/// What a [`WindowKind::AnchoredPopup`] keeps between placements.
///
/// The popup is an override-redirect top-level, as a menu is, that this backend positions
/// itself: at creation, whenever the parent moves (the parent's ConfigureNotify), when the
/// anchor changes and when it is resized. It never has the X input focus: the parent keeps
/// it and stays the active window, and the client sends the parent's keystrokes to the popup
/// that asked for the keyboard until a press on the parent takes it back.
/// `PopupOptions::grab` has no effect on X11.
struct PopupState {
    options: PopupOptions,
    parent: X11WindowStatePtr,
    /// The size asked for, kept apart from the window's bounds so a placement made before the
    /// server confirms a resize does not put the old size back, and so a popup shrunk to fit by
    /// a constraint adjustment grows again when there is room.
    size: Size<Pixels>,
    focus: PopupFocus,
}

enum PopupFocus {
    /// Not asked for, or already given.
    Done,
    /// Asked for; waiting for the server timestamp requested on map.
    AwaitingTimestamp,
    /// Asked for, with a timestamp; waiting for the window to be viewable, since SetInputFocus
    /// on a window that is not is a BadMatch.
    Pending(xproto::Timestamp),
}

/// Where a popup of `size` goes, in device pixels relative to the root window.
///
/// The parent's origin comes from TranslateCoordinates rather than its ConfigureNotify, because
/// under a reparenting window manager a ConfigureNotify that is not synthetic is relative to
/// the frame. Placement itself happens in logical pixels, the space `PopupOptions` is in, using
/// the backend's single scale factor for both directions.
fn place_popup_on_screen(
    xcb: &XCBConnection,
    atoms: &XcbAtoms,
    x_screen_index: usize,
    parent: xproto::Window,
    options: &PopupOptions,
    size: Size<Pixels>,
    scale_factor: f32,
) -> anyhow::Result<Bounds<DevicePixels>> {
    let root = xcb.setup().roots[x_screen_index].root;
    let origin = get_reply(
        || "X11 TranslateCoordinates for the parent of a popup failed.",
        xcb.translate_coordinates(parent, root, 0, 0),
    )?;
    let to_logical = |value: i32| px(value as f32 / scale_factor);
    let parent_origin = Point::new(
        to_logical(origin.dst_x as i32),
        to_logical(origin.dst_y as i32),
    );
    let anchor = options.anchor_rect.center();
    let anchor = Point::new(
        origin.dst_x as i32 + (f32::from(anchor.x) * scale_factor).round() as i32,
        origin.dst_y as i32 + (f32::from(anchor.y) * scale_factor).round() as i32,
    );
    let work_area = work_area_at(xcb, atoms, x_screen_index, anchor);
    let work_area = Bounds {
        origin: Point::new(
            to_logical(work_area.origin.x),
            to_logical(work_area.origin.y),
        ),
        size: Size {
            width: to_logical(work_area.size.width),
            height: to_logical(work_area.size.height),
        },
    };
    let placed = place_popup(options, parent_origin, size, Some(work_area));
    let to_device = |value: Pixels| (f32::from(value) * scale_factor).round() as i32;
    Ok(Bounds {
        origin: Point::new(
            DevicePixels(to_device(placed.origin.x)),
            DevicePixels(to_device(placed.origin.y)),
        ),
        size: Size {
            width: DevicePixels(to_device(placed.size.width).max(1)),
            height: DevicePixels(to_device(placed.size.height).max(1)),
        },
    })
}

/// The usable part of the monitor at `point` (root coordinates, device pixels): the RandR
/// monitor holding it, or the nearest one, less what `_NET_WORKAREA` reserves for panels. The
/// whole screen when RandR 1.5 is missing. `_NET_WORKAREA` is one rectangle across every
/// monitor, so a panel on one monitor only trims another if it lies along the shared edge.
fn work_area_at(
    xcb: &XCBConnection,
    atoms: &XcbAtoms,
    x_screen_index: usize,
    point: Point<i32>,
) -> Bounds<i32> {
    let screen = &xcb.setup().roots[x_screen_index];
    let root = screen.root;
    let whole_screen = Bounds {
        origin: Point::new(0, 0),
        size: Size {
            width: screen.width_in_pixels as i32,
            height: screen.height_in_pixels as i32,
        },
    };

    let monitor = xcb
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .and_then(|reply| {
            let monitors = reply.monitors.iter().map(|monitor| Bounds {
                origin: Point::new(monitor.x as i32, monitor.y as i32),
                size: Size {
                    width: monitor.width as i32,
                    height: monitor.height as i32,
                },
            });
            let distance = |bounds: &Bounds<i32>| {
                let dx = (bounds.origin.x - point.x)
                    .max(point.x - (bounds.origin.x + bounds.size.width))
                    .max(0) as i64;
                let dy = (bounds.origin.y - point.y)
                    .max(point.y - (bounds.origin.y + bounds.size.height))
                    .max(0) as i64;
                dx * dx + dy * dy
            };
            monitors.min_by_key(distance)
        })
        .unwrap_or(whole_screen);

    let work_area = maybe!({
        let desktop = get_reply(
            || "X11 GetProperty for _NET_CURRENT_DESKTOP failed.",
            xcb.get_property(
                false,
                root,
                atoms._NET_CURRENT_DESKTOP,
                xproto::AtomEnum::CARDINAL,
                0,
                1,
            ),
        )?
        .value32()
        .and_then(|mut values| values.next())
        .unwrap_or(0);
        let reply = get_reply(
            || "X11 GetProperty for _NET_WORKAREA failed.",
            xcb.get_property(
                false,
                root,
                atoms._NET_WORKAREA,
                xproto::AtomEnum::CARDINAL,
                desktop * 4,
                4,
            ),
        )?;
        let values: Vec<u32> = reply
            .value32()
            .map(|values| values.collect())
            .unwrap_or_default();
        let [x, y, width, height] = values[..] else {
            anyhow::bail!("_NET_WORKAREA has no entry for desktop {desktop}");
        };
        anyhow::Ok(Bounds {
            origin: Point::new(x as i32, y as i32),
            size: Size {
                width: width as i32,
                height: height as i32,
            },
        })
    });

    match work_area {
        Ok(work_area) => {
            let usable = monitor.intersect(&work_area);
            if usable.size.width > 0 && usable.size.height > 0 {
                usable
            } else {
                monitor
            }
        }
        Err(_) => monitor,
    }
}

/// WM_NORMAL_HINTS for a popup: a user-specified position, so the window manager keeps the one
/// it is given instead of placing the window itself (mutter and kwin centre transient dialogs
/// otherwise), a fixed size, and static gravity, so the position is the client area's even
/// under a window manager that decorates it anyway.
fn popup_size_hints(bounds: Bounds<DevicePixels>) -> WmSizeHints {
    let (x, y) = (bounds.origin.x.0, bounds.origin.y.0);
    let (width, height) = (bounds.size.width.0, bounds.size.height.0);
    let mut hints = WmSizeHints::new();
    hints.position = Some((WmSizeHintsSpecification::UserSpecified, x, y));
    hints.size = Some((WmSizeHintsSpecification::UserSpecified, width, height));
    hints.min_size = Some((width, height));
    hints.max_size = Some((width, height));
    hints.win_gravity = Some(xproto::Gravity::STATIC);
    hints
}

/// _MOTIF_WM_HINTS with the decorations flag set and no decorations.
const MOTIF_NO_DECORATIONS: [u32; 5] = [1 << 1, 0, 0, 0, 0];

#[derive(Debug, Clone, Copy)]
struct RawWindow {
    connection: *mut c_void,
    screen_id: usize,
    window_id: u32,
    visual_id: u32,
}

// Safety: The raw pointers in RawWindow point to X11 connection
// which is valid for the window's lifetime. These are used only for
// passing to wgpu which needs Send+Sync for surface creation.
unsafe impl Send for RawWindow {}
unsafe impl Sync for RawWindow {}

#[derive(Default)]
pub struct Callbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> gpui::DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    visibility_change: Option<Box<dyn FnMut(WindowVisibility)>>,
    hovered_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
    button_layout_changed: Option<Box<dyn FnMut()>>,
}

pub struct X11WindowState {
    parent: Option<X11WindowStatePtr>,
    children: FxHashSet<xproto::Window>,
    /// Set on an anchored popup.
    popup: Option<PopupState>,
    /// The anchored popups this window is the parent of. Unlike `children` they do not block
    /// input; they follow this window and close with it.
    popups: FxHashSet<xproto::Window>,
    /// The popup of this window that keystrokes go to while this window has the X input
    /// focus.
    keyboard_popup: Option<xproto::Window>,
    /// The keyboard focus last reported to GPUI, so that focus moving between this window and
    /// its own popups is not reported as it going inactive and straight back.
    reported_active: Option<bool>,
    client: X11ClientStatePtr,
    executor: ForegroundExecutor,
    atoms: XcbAtoms,
    x_root_window: xproto::Window,
    x_screen_index: usize,
    visual_id: u32,
    pub(crate) counter_id: sync::Counter,
    pub(crate) last_sync_counter: Option<sync::Int64>,
    bounds: Bounds<Pixels>,
    scale_factor: f32,
    /// Taken when the window is dropped. Its GPU objects use the X connection, so they mustn't
    /// outlive the window, which a display mode switch relies on.
    renderer: Option<WgpuRenderer>,
    display: Rc<dyn PlatformDisplay>,
    input_handler: Option<PlatformInputHandler>,
    appearance: WindowAppearance,
    background_appearance: WindowBackgroundAppearance,
    maximized_vertical: bool,
    maximized_horizontal: bool,
    visible: bool,
    hidden: bool,
    active: bool,
    /// Owned by the client's `WindowRef`, which combines the mapped state with
    /// `VisibilityNotify`; this is the last value it reported.
    visibility: WindowVisibility,
    hovered: bool,
    force_render_after_recovery: bool,
    fullscreen: bool,
    client_side_decorations_supported: bool,
    decorations: WindowDecorations,
    edge_constraints: Option<EdgeConstraints>,
    pub handle: AnyWindowHandle,
    last_insets: [u32; 4],
    accesskit_adapter: Option<accesskit_unix::Adapter>,
}

impl X11WindowState {
    fn is_transparent(&self) -> bool {
        self.background_appearance.is_transparent()
    }

    fn update_transparency(&mut self) {
        let is_transparent = self.is_transparent();
        if let Some(renderer) = &mut self.renderer {
            renderer.update_transparency(is_transparent);
        }
    }
}

#[derive(Clone)]
pub(crate) struct X11WindowStatePtr {
    pub state: Rc<RefCell<X11WindowState>>,
    pub(crate) callbacks: Rc<RefCell<Callbacks>>,
    xcb: Rc<XCBConnection>,
    pub(crate) x_window: xproto::Window,
}

impl rwh::HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let Some(non_zero) = NonZeroU32::new(self.window_id) else {
            log::error!("RawWindow.window_id zero when getting window handle.");
            return Err(rwh::HandleError::Unavailable);
        };
        let mut handle = rwh::XcbWindowHandle::new(non_zero);
        handle.visual_id = NonZeroU32::new(self.visual_id);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}
impl rwh::HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let Some(non_zero) = NonNull::new(self.connection) else {
            log::error!("Null RawWindow.connection when getting display handle.");
            return Err(rwh::HandleError::Unavailable);
        };
        let handle = rwh::XcbDisplayHandle::new(Some(non_zero), self.screen_id as i32);
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(handle.into()) })
    }
}

impl rwh::HasWindowHandle for X11Window {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let Some(non_zero) = NonZeroU32::new(self.0.x_window) else {
            return Err(rwh::HandleError::Unavailable);
        };
        let handle = rwh::XcbWindowHandle::new(non_zero);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl rwh::HasDisplayHandle for X11Window {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let connection =
            as_raw_xcb_connection::AsRawXcbConnection::as_raw_xcb_connection(&*self.0.xcb)
                as *mut _;
        let Some(non_zero) = NonNull::new(connection) else {
            return Err(rwh::HandleError::Unavailable);
        };
        let screen_id = {
            let state = self.0.state.borrow();
            u64::from(state.display.id()) as i32
        };
        let handle = rwh::XcbDisplayHandle::new(Some(non_zero), screen_id);
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(handle.into()) })
    }
}

pub(crate) fn xcb_flush(xcb: &XCBConnection) {
    xcb.flush()
        .map_err(handle_connection_error)
        .context("X11 flush failed")
        .log_err();
}

pub(crate) fn check_reply<E, F, C>(
    failure_context: F,
    result: Result<VoidCookie<'_, C>, ConnectionError>,
) -> anyhow::Result<()>
where
    E: Display + Send + Sync + 'static,
    F: FnOnce() -> E,
    C: RequestConnection,
{
    result
        .map_err(handle_connection_error)
        .and_then(|response| response.check().map_err(|reply_error| anyhow!(reply_error)))
        .with_context(failure_context)
}

pub(crate) fn get_reply<E, F, C, O>(
    failure_context: F,
    result: Result<Cookie<'_, C, O>, ConnectionError>,
) -> anyhow::Result<O>
where
    E: Display + Send + Sync + 'static,
    F: FnOnce() -> E,
    C: RequestConnection,
    O: x11rb::x11_utils::TryParse,
{
    result
        .map_err(handle_connection_error)
        .and_then(|response| response.reply().map_err(|reply_error| anyhow!(reply_error)))
        .with_context(failure_context)
}

/// Sets or clears the ICCCM WM_HINTS urgency flag, preserving the other hints.
///
/// Clearing when the flag isn't set is skipped: writing it back would create a
/// WM_HINTS property on windows that never requested attention, and would add an
/// X round trip to every window state change.
fn set_wm_hints_urgency(xcb: &XCBConnection, x_window: xproto::Window, urgent: bool) {
    let mut hints = WmHints::new();
    match WmHints::get(xcb, x_window) {
        Ok(cookie) => match cookie.reply() {
            Ok(Some(existing_hints)) => hints = existing_hints,
            Ok(None) => {}
            Err(error) => {
                log::debug!("failed to read X11 WM_HINTS before setting urgency: {error}")
            }
        },
        Err(error) => {
            log::debug!("failed to request X11 WM_HINTS before setting urgency: {error}")
        }
    }

    if !urgent && !hints.urgent {
        return;
    }

    hints.urgent = urgent;
    check_reply(
        || "X11 ChangeProperty for WM_HINTS urgency failed.",
        hints.set(xcb, x_window),
    )
    .log_err();
    xcb_flush(xcb);
}

/// Convert X11 connection errors to `anyhow::Error` and panic for unrecoverable errors.
pub(crate) fn handle_connection_error(err: ConnectionError) -> anyhow::Error {
    match err {
        ConnectionError::UnknownError => anyhow!("X11 connection: Unknown error"),
        ConnectionError::UnsupportedExtension => anyhow!("X11 connection: Unsupported extension"),
        ConnectionError::MaximumRequestLengthExceeded => {
            anyhow!("X11 connection: Maximum request length exceeded")
        }
        ConnectionError::FdPassingFailed => {
            panic!("X11 connection: File descriptor passing failed")
        }
        ConnectionError::ParseError(parse_error) => {
            anyhow!(parse_error).context("Parse error in X11 response")
        }
        ConnectionError::InsufficientMemory => panic!("X11 connection: Insufficient memory"),
        ConnectionError::IoError(err) => anyhow!(err).context("X11 connection: IOError"),
        _ => anyhow!(err),
    }
}

impl X11WindowState {
    pub fn new(
        handle: AnyWindowHandle,
        client: X11ClientStatePtr,
        executor: ForegroundExecutor,
        gpu_context: gpui_wgpu::GpuContext,
        compositor_gpu: Option<CompositorGpuHint>,
        gpu_requirements: Option<gpui_wgpu::WgpuDeviceRequirements>,
        params: WindowParams,
        xcb: &Rc<XCBConnection>,
        client_side_decorations_supported: bool,
        x_main_screen_index: usize,
        x_window: xproto::Window,
        atoms: &XcbAtoms,
        scale_factor: f32,
        appearance: WindowAppearance,
        parent_window: Option<X11WindowStatePtr>,
        supports_xinput_gestures: bool,
        is_bgr: bool,
    ) -> anyhow::Result<Self> {
        // For an anchored popup `parent_window` is the parent its options name, not the
        // focused window: the client resolves it from `PopupOptions::parent`.
        let popup = match &params.kind {
            WindowKind::AnchoredPopup(options) => Some((
                options.clone(),
                parent_window
                    .clone()
                    .context("X11: the parent of an anchored popup is not an open window")?,
            )),
            _ => None,
        };
        let focus_on_map = params.focus;

        let x_screen_index = match &popup {
            Some((_, parent)) => parent.state.borrow().x_screen_index,
            None => params
                .display_id
                .map_or(x_main_screen_index, |did| u64::from(did) as usize),
        };

        let visual_set = find_visuals(xcb, x_screen_index);

        let visual = match visual_set.transparent {
            Some(visual) => visual,
            None => {
                log::warn!("Unable to find a transparent visual",);
                visual_set.inherit
            }
        };
        log::info!("Using {:?}", visual);

        let colormap = if visual.colormap != 0 {
            visual.colormap
        } else {
            let id = xcb.generate_id()?;
            log::info!("Creating colormap {}", id);
            check_reply(
                || format!("X11 CreateColormap failed. id: {}", id),
                xcb.create_colormap(xproto::ColormapAlloc::NONE, id, visual_set.root, visual.id),
            )?;
            id
        };

        let win_aux = xproto::CreateWindowAux::new()
            // https://stackoverflow.com/questions/43218127/x11-xlib-xcb-creating-a-window-requires-border-pixel-if-specifying-colormap-wh
            .border_pixel(visual_set.black_pixel)
            .colormap(colormap)
            // An anchored popup is override-redirect, as a menu is: the window manager neither
            // focuses, raises nor animates it, so the parent stays the active window under it
            // and the popups keep the order they were mapped in, a scrim under the popup opened
            // after it. A managed popup takes the focus, and the parent is drawn inactive.
            .override_redirect((params.kind == WindowKind::PopUp || popup.is_some()) as u32)
            .event_mask(
                xproto::EventMask::EXPOSURE
                    | xproto::EventMask::STRUCTURE_NOTIFY
                    | xproto::EventMask::FOCUS_CHANGE
                    | xproto::EventMask::KEY_PRESS
                    | xproto::EventMask::KEY_RELEASE
                    | xproto::EventMask::PROPERTY_CHANGE
                    | xproto::EventMask::VISIBILITY_CHANGE,
            );

        let mut bounds = params.bounds.to_device_pixels(scale_factor);
        if bounds.size.width.0 == 0 || bounds.size.height.0 == 0 {
            log::warn!(
                "Window bounds contain a zero value. height={}, width={}. Falling back to defaults.",
                bounds.size.height.0,
                bounds.size.width.0
            );
            bounds.size.width = 800.into();
            bounds.size.height = 600.into();
        }

        let popup_size = Size {
            width: px(bounds.size.width.0 as f32 / scale_factor),
            height: px(bounds.size.height.0 as f32 / scale_factor),
        };
        if let Some((options, parent)) = &popup {
            bounds = place_popup_on_screen(
                xcb,
                atoms,
                x_screen_index,
                parent.x_window,
                options,
                popup_size,
                scale_factor,
            )?;
        }
        // Other windows are created 2px right of where they were asked for, part of the
        // workaround below for content drawn outside the window at the default position on
        // GNOME. A popup's position is computed against its anchor and pinned with
        // USPosition, so it is neither at the default position nor free to be nudged.
        let nudge_x = if popup.is_some() { 0 } else { 2 };

        check_reply(
            || {
                format!(
                    "X11 CreateWindow failed. depth: {}, x_window: {}, visual_set.root: {}, bounds.origin.x.0: {}, bounds.origin.y.0: {}, bounds.size.width.0: {}, bounds.size.height.0: {}",
                    visual.depth,
                    x_window,
                    visual_set.root,
                    bounds.origin.x.0 + nudge_x,
                    bounds.origin.y.0,
                    bounds.size.width.0,
                    bounds.size.height.0
                )
            },
            xcb.create_window(
                visual.depth,
                x_window,
                visual_set.root,
                (bounds.origin.x.0 + nudge_x) as i16,
                bounds.origin.y.0 as i16,
                bounds.size.width.0 as u16,
                bounds.size.height.0 as u16,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                visual.id,
                &win_aux,
            ),
        )?;

        // Collect errors during setup, so that window can be destroyed on failure.
        let setup_result = maybe!({
            let pid = std::process::id();
            check_reply(
                || "X11 ChangeProperty for _NET_WM_PID failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms._NET_WM_PID,
                    xproto::AtomEnum::CARDINAL,
                    &[pid],
                ),
            )?;

            let reply = get_reply(|| "X11 GetGeometry failed.", xcb.get_geometry(x_window))?;
            if popup.is_none() && reply.x == 0 && reply.y == 0 {
                bounds.origin.x.0 += 2;
                // Work around a bug where our rendered content appears
                // outside the window bounds when opened at the default position
                // (14px, 49px on X + Gnome + Ubuntu 22).
                let x = bounds.origin.x.0;
                let y = bounds.origin.y.0;
                check_reply(
                    || format!("X11 ConfigureWindow failed. x: {}, y: {}", x, y),
                    xcb.configure_window(x_window, &xproto::ConfigureWindowAux::new().x(x).y(y)),
                )?;
            }
            if let Some(titlebar) = params.titlebar
                && let Some(title) = titlebar.title
            {
                check_reply(
                    || "X11 ChangeProperty8 on WM_NAME failed.",
                    xcb.change_property8(
                        xproto::PropMode::REPLACE,
                        x_window,
                        xproto::AtomEnum::WM_NAME,
                        xproto::AtomEnum::STRING,
                        title.as_bytes(),
                    ),
                )?;
                check_reply(
                    || "X11 ChangeProperty8 on _NET_WM_NAME failed.",
                    xcb.change_property8(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_NAME,
                        atoms.UTF8_STRING,
                        title.as_bytes(),
                    ),
                )?;
            }

            if params.kind == WindowKind::PopUp {
                check_reply(
                    || "X11 ChangeProperty32 setting window type for pop-up failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_WINDOW_TYPE,
                        xproto::AtomEnum::ATOM,
                        &[atoms._NET_WM_WINDOW_TYPE_NOTIFICATION],
                    ),
                )?;
            }

            if params.kind == WindowKind::Floating || params.kind == WindowKind::Dialog {
                if let Some(parent_window) = parent_window.as_ref().map(|w| w.x_window) {
                    // WM_TRANSIENT_FOR hint indicating the main application window. For floating windows, we set
                    // a parent window (WM_TRANSIENT_FOR) such that the window manager knows where to
                    // place the floating window in relation to the main window.
                    // https://specifications.freedesktop.org/wm-spec/1.4/ar01s05.html
                    check_reply(
                        || "X11 ChangeProperty32 setting WM_TRANSIENT_FOR for floating window failed.",
                        xcb.change_property32(
                            xproto::PropMode::REPLACE,
                            x_window,
                            atoms.WM_TRANSIENT_FOR,
                            xproto::AtomEnum::WINDOW,
                            &[parent_window],
                        ),
                    )?;
                }
            }

            if let Some((_, parent)) = &popup {
                // The window manager does not manage an override-redirect window; these say
                // what it is to a compositor and to tools that read them. Transient for the
                // parent named in the options, never the focused window.
                check_reply(
                    || "X11 ChangeProperty32 setting WM_TRANSIENT_FOR for a popup failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms.WM_TRANSIENT_FOR,
                        xproto::AtomEnum::WINDOW,
                        &[parent.x_window],
                    ),
                )?;
                check_reply(
                    || "X11 ChangeProperty32 setting window type for a popup failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_WINDOW_TYPE,
                        xproto::AtomEnum::ATOM,
                        &[atoms._NET_WM_WINDOW_TYPE_POPUP_MENU],
                    ),
                )?;
                check_reply(
                    || "X11 ChangeProperty32 setting the state of a popup failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_STATE,
                        xproto::AtomEnum::ATOM,
                        &[
                            atoms._NET_WM_STATE_SKIP_TASKBAR,
                            atoms._NET_WM_STATE_SKIP_PAGER,
                        ],
                    ),
                )?;
                check_reply(
                    || "X11 ChangeProperty for _MOTIF_WM_HINTS on a popup failed.",
                    xcb.change_property(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._MOTIF_WM_HINTS,
                        atoms._MOTIF_WM_HINTS,
                        size_of::<u32>() as u8 * 8,
                        5,
                        bytemuck::cast_slice::<u32, u8>(&MOTIF_NO_DECORATIONS),
                    ),
                )?;
                // Never the X input focus, even for a popup that takes the keyboard: its
                // parent keeps the focus and this backend hands the popup the keystrokes, the
                // way a menu gets them.
                let mut wm_hints = WmHints::new();
                wm_hints.input = Some(false);
                check_reply(
                    || "X11 ChangeProperty for WM_HINTS on a popup failed.",
                    wm_hints.set(xcb, x_window),
                )?;
            }

            let parent = if params.kind == WindowKind::Dialog
                && let Some(parent) = parent_window
            {
                parent.add_child(x_window);

                Some(parent)
            } else {
                None
            };

            if params.kind == WindowKind::Dialog {
                // _NET_WM_WINDOW_TYPE_DIALOG indicates that this is a dialog (floating) window
                // https://specifications.freedesktop.org/wm-spec/1.4/ar01s05.html
                check_reply(
                    || "X11 ChangeProperty32 setting window type for dialog window failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_WINDOW_TYPE,
                        xproto::AtomEnum::ATOM,
                        &[atoms._NET_WM_WINDOW_TYPE_DIALOG],
                    ),
                )?;

                // We set the modal state for dialog windows, so that the window manager
                // can handle it appropriately (e.g., prevent interaction with the parent window
                // while the dialog is open).
                check_reply(
                    || "X11 ChangeProperty32 setting modal state for dialog window failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_STATE,
                        xproto::AtomEnum::ATOM,
                        &[atoms._NET_WM_STATE_MODAL],
                    ),
                )?;
            }

            check_reply(
                || "X11 ChangeProperty32 setting protocols failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms.WM_PROTOCOLS,
                    xproto::AtomEnum::ATOM,
                    &[atoms.WM_DELETE_WINDOW, atoms._NET_WM_SYNC_REQUEST],
                ),
            )?;

            get_reply(
                || "X11 sync protocol initialize failed.",
                sync::initialize(xcb, 3, 1),
            )?;
            let sync_request_counter = xcb.generate_id()?;
            check_reply(
                || "X11 sync CreateCounter failed.",
                sync::create_counter(xcb, sync_request_counter, sync::Int64 { lo: 0, hi: 0 }),
            )?;

            check_reply(
                || "X11 ChangeProperty32 setting sync request counter failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms._NET_WM_SYNC_REQUEST_COUNTER,
                    xproto::AtomEnum::CARDINAL,
                    &[sync_request_counter],
                ),
            )?;

            let mut xi_event_mask = xinput::XIEventMask::MOTION
                | xinput::XIEventMask::BUTTON_PRESS
                | xinput::XIEventMask::BUTTON_RELEASE
                | xinput::XIEventMask::ENTER
                | xinput::XIEventMask::LEAVE;
            if supports_xinput_gestures {
                // x11rb 0.13 doesn't define XIEventMask constants for gesture
                // events, so we construct them from the event opcodes (each
                // XInput event type N maps to mask bit N).
                xi_event_mask |=
                    xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_BEGIN_EVENT)
                        | xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_UPDATE_EVENT)
                        | xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_END_EVENT);
            }
            check_reply(
                || "X11 XiSelectEvents failed.",
                xcb.xinput_xi_select_events(
                    x_window,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICE_GROUPS,
                        mask: vec![xi_event_mask],
                    }],
                ),
            )?;

            check_reply(
                || "X11 XiSelectEvents for device changes failed.",
                xcb.xinput_xi_select_events(
                    x_window,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICES,
                        mask: vec![
                            xinput::XIEventMask::HIERARCHY | xinput::XIEventMask::DEVICE_CHANGED,
                        ],
                    }],
                ),
            )?;

            xcb_flush(xcb);

            let mut renderer = {
                let raw_window = RawWindow {
                    connection: as_raw_xcb_connection::AsRawXcbConnection::as_raw_xcb_connection(
                        xcb,
                    ) as *mut _,
                    screen_id: x_screen_index,
                    window_id: x_window,
                    visual_id: visual.id,
                };
                let config = WgpuSurfaceConfig {
                    // Note: this has to be done after the GPU init, or otherwise
                    // the sizes are immediately invalidated.
                    size: query_render_extent(xcb, x_window)?,
                    // We set it to transparent by default, even if we have client-side
                    // decorations, since those seem to work on X11 even without `true` here.
                    // If the window appearance changes, then the renderer will get updated
                    // too
                    transparent: false,
                    preferred_present_mode: None,
                };
                WgpuRenderer::new(
                    gpu_context,
                    &raw_window,
                    config,
                    compositor_gpu,
                    gpu_requirements,
                )?
            };

            renderer.set_subpixel_layout(is_bgr);

            // Set max window size hints based on the GPU's maximum texture dimension.
            // This prevents the window from being resized larger than what the GPU can render.
            let max_texture_size = renderer.max_texture_size();
            let mut size_hints = WmSizeHints::new();
            if popup.is_some() {
                size_hints = popup_size_hints(bounds);
            } else {
                if let Some(size) = params.window_min_size {
                    size_hints.min_size =
                        Some((f32::from(size.width) as i32, f32::from(size.height) as i32));
                }
                size_hints.max_size = Some((max_texture_size as i32, max_texture_size as i32));
            }
            check_reply(
                || {
                    format!(
                        "X11 change of WM_SIZE_HINTS failed. max_size: {:?}",
                        max_texture_size
                    )
                },
                size_hints.set_normal_hints(xcb, x_window),
            )?;

            if let Some(image) = params.icon {
                // https://specifications.freedesktop.org/wm-spec/1.4/ar01s05.html#id-1.6.13
                let property_size = 2 + (image.width() * image.height()) as usize;
                let mut property_data: Vec<u32> = Vec::with_capacity(property_size);
                property_data.push(image.width());
                property_data.push(image.height());
                property_data.extend(image.pixels().map(|px| {
                    let [r, g, b, a]: [u8; 4] = px.0;
                    u32::from_le_bytes([b, g, r, a])
                }));

                check_reply(
                    || "X11 ChangeProperty32 for _NET_ICON_NAME failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_ICON,
                        xproto::AtomEnum::CARDINAL,
                        &property_data,
                    ),
                )?;
            }

            let display = Rc::new(X11Display::new(xcb, scale_factor, x_screen_index)?);

            let popup = popup.map(|(options, parent)| {
                parent.state.borrow_mut().popups.insert(x_window);
                PopupState {
                    options,
                    parent,
                    size: popup_size,
                    focus: if focus_on_map {
                        PopupFocus::AwaitingTimestamp
                    } else {
                        PopupFocus::Done
                    },
                }
            });

            Ok(Self {
                parent,
                children: FxHashSet::default(),
                popup,
                popups: FxHashSet::default(),
                keyboard_popup: None,
                reported_active: None,
                client,
                executor,
                display,
                x_root_window: visual_set.root,
                x_screen_index,
                visual_id: visual.id,
                bounds: bounds.to_pixels(scale_factor),
                scale_factor,
                renderer: Some(renderer),
                atoms: *atoms,
                input_handler: None,
                active: false,
                // The window is not mapped until the client sees `MapNotify`.
                visibility: WindowVisibility::Hidden,
                hovered: false,
                force_render_after_recovery: false,
                fullscreen: false,
                maximized_vertical: false,
                maximized_horizontal: false,
                visible: params.show,
                hidden: false,
                appearance,
                handle,
                background_appearance: WindowBackgroundAppearance::Opaque,
                client_side_decorations_supported,
                decorations: WindowDecorations::Server,
                last_insets: [0, 0, 0, 0],
                edge_constraints: None,
                accesskit_adapter: None,
                counter_id: sync_request_counter,
                last_sync_counter: None,
            })
        });

        if setup_result.is_err() {
            check_reply(
                || "X11 DestroyWindow failed while cleaning it up after setup failure.",
                xcb.destroy_window(x_window),
            )?;
            xcb_flush(xcb);
        }

        setup_result
    }

    fn content_size(&self) -> Size<Pixels> {
        self.bounds.size
    }
}

pub(crate) struct X11Window(pub X11WindowStatePtr);

impl Drop for X11Window {
    fn drop(&mut self) {
        let mut state = self.0.state.borrow_mut();

        if let Some(parent) = state.parent.as_ref() {
            parent.state.borrow_mut().children.remove(&self.0.x_window);
        }

        if let Some(popup) = state.popup.as_ref() {
            let parent = &popup.parent;
            let mut parent_state = parent.state.borrow_mut();
            parent_state.popups.remove(&self.0.x_window);
            if parent_state.keyboard_popup == Some(self.0.x_window) {
                parent_state.keyboard_popup = None;
            }
            drop(parent_state);
            // Hand the keyboard to the parent rather than leave it to the revert-to of a
            // destroyed window, which some window managers resolve to whatever is under the
            // pointer or to nothing at all. A native child of the parent that held the
            // keyboard before the popup opened gets it back only if the host asks for it.
            // A closing window leaves the client's map before its popups close, so a parent
            // still in it is still open.
            let (popup_has_keyboard, parent_open) = state
                .client
                .0
                .upgrade()
                .and_then(|client| {
                    client.try_borrow().ok().map(|client| {
                        (
                            client.keyboard_focused_window == Some(self.0.x_window),
                            client.windows.contains_key(&parent.x_window),
                        )
                    })
                })
                .unwrap_or((false, false));
            if popup_has_keyboard && parent_open {
                self.0
                    .xcb
                    .set_input_focus(
                        xproto::InputFocus::POINTER_ROOT,
                        parent.x_window,
                        xproto::Time::CURRENT_TIME,
                    )
                    .log_err();
            }
        }

        // The renderer's GPU objects use the X connection, and a display mode switch closes the
        // connection once no window is left in the client's window map, so both go now.
        state.renderer.take();

        maybe!({
            check_reply(
                || "X11 DestroyWindow failure.",
                self.0.xcb.destroy_window(self.0.x_window),
            )?;
            xcb_flush(&self.0.xcb);

            anyhow::Ok(())
        })
        .log_err();
        let client = state.client.clone();
        let executor = state.executor.clone();
        drop(state);

        client.drop_window(self.0.x_window);
        let this_ptr = self.0.clone();
        executor.spawn(async move { this_ptr.close() }).detach();
    }
}

enum WmHintPropertyState {
    // Remove = 0,
    // Add = 1,
    Toggle = 2,
}

impl X11Window {
    pub fn new(
        handle: AnyWindowHandle,
        client: X11ClientStatePtr,
        executor: ForegroundExecutor,
        gpu_context: gpui_wgpu::GpuContext,
        compositor_gpu: Option<CompositorGpuHint>,
        gpu_requirements: Option<gpui_wgpu::WgpuDeviceRequirements>,
        params: WindowParams,
        xcb: &Rc<XCBConnection>,
        client_side_decorations_supported: bool,
        x_main_screen_index: usize,
        x_window: xproto::Window,
        atoms: &XcbAtoms,
        scale_factor: f32,
        appearance: WindowAppearance,
        parent_window: Option<X11WindowStatePtr>,
        supports_xinput_gestures: bool,
        is_bgr: bool,
    ) -> anyhow::Result<Self> {
        let ptr = X11WindowStatePtr {
            state: Rc::new(RefCell::new(X11WindowState::new(
                handle,
                client,
                executor,
                gpu_context,
                compositor_gpu,
                gpu_requirements,
                params,
                xcb,
                client_side_decorations_supported,
                x_main_screen_index,
                x_window,
                atoms,
                scale_factor,
                appearance,
                parent_window,
                supports_xinput_gestures,
                is_bgr,
            )?)),
            callbacks: Rc::new(RefCell::new(Callbacks::default())),
            xcb: xcb.clone(),
            x_window,
        };

        let state = ptr.state.borrow_mut();
        ptr.set_wm_properties(state)?;

        Ok(Self(ptr))
    }

    fn set_wm_hints<C: Display + Send + Sync + 'static, F: FnOnce() -> C>(
        &self,
        failure_context: F,
        wm_hint_property_state: WmHintPropertyState,
        prop1: u32,
        prop2: u32,
    ) -> anyhow::Result<()> {
        let state = self.0.state.borrow();
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms._NET_WM_STATE,
            [wm_hint_property_state as u32, prop1, prop2, 1, 0],
        );
        check_reply(
            failure_context,
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )?;
        xcb_flush(&self.0.xcb);
        Ok(())
    }

    fn get_root_position(
        &self,
        position: Point<Pixels>,
    ) -> anyhow::Result<TranslateCoordinatesReply> {
        let state = self.0.state.borrow();
        get_reply(
            || "X11 TranslateCoordinates failed.",
            self.0.xcb.translate_coordinates(
                self.0.x_window,
                state.x_root_window,
                (f32::from(position.x) * state.scale_factor) as i16,
                (f32::from(position.y) * state.scale_factor) as i16,
            ),
        )
    }

    fn send_moveresize(&self, flag: u32) -> anyhow::Result<()> {
        let state = self.0.state.borrow();

        check_reply(
            || "X11 UngrabPointer before move/resize of window failed.",
            self.0.xcb.ungrab_pointer(x11rb::CURRENT_TIME),
        )?;

        let pointer = get_reply(
            || "X11 QueryPointer before move/resize of window failed.",
            self.0.xcb.query_pointer(self.0.x_window),
        )?;
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms._NET_WM_MOVERESIZE,
            [
                pointer.root_x as u32,
                pointer.root_y as u32,
                flag,
                0, // Left mouse button
                0,
            ],
        );
        check_reply(
            || "X11 SendEvent to move/resize window failed.",
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )?;

        xcb_flush(&self.0.xcb);
        Ok(())
    }
}

impl X11WindowStatePtr {
    pub fn should_close(&self) -> bool {
        let mut cb = self.callbacks.borrow_mut();
        if let Some(mut should_close) = cb.should_close.take() {
            let result = (should_close)();
            cb.should_close = Some(should_close);
            result
        } else {
            true
        }
    }

    pub fn property_notify(&self, event: xproto::PropertyNotifyEvent) -> anyhow::Result<()> {
        let state = self.state.borrow_mut();
        if event.atom == state.atoms._NET_WM_STATE {
            self.set_wm_properties(state)?;
        } else if event.atom == state.atoms._GTK_EDGE_CONSTRAINTS {
            self.set_edge_constraints(state)?;
        } else if event.atom == state.atoms._GPUI_TIMESTAMP {
            drop(state);
            self.popup_timestamp(event.time);
        } else if event.atom == state.atoms.WM_STATE && !state.popups.is_empty() {
            // ICCCM: every window manager sets WM_STATE. IconicState (3) is the window
            // minimized, and some window managers (i3) set WithdrawnState (0) on a window of a
            // workspace that is not showing, without unmapping it. The popups are not the
            // window manager's to hide, so close them here instead of leaving one over nothing.
            let reply = get_reply(
                || "X11 GetProperty for WM_STATE failed.",
                self.xcb.get_property(
                    false,
                    self.x_window,
                    state.atoms.WM_STATE,
                    xproto::AtomEnum::ANY,
                    0,
                    1,
                ),
            )?;
            drop(state);
            const NORMAL_STATE: u32 = 1;
            let hidden = reply
                .value32()
                .and_then(|mut values| values.next())
                .is_some_and(|value| value != NORMAL_STATE);
            if hidden {
                self.close_popups();
            }
        }
        Ok(())
    }

    fn set_edge_constraints(
        &self,
        mut state: std::cell::RefMut<X11WindowState>,
    ) -> anyhow::Result<()> {
        let reply = get_reply(
            || "X11 GetProperty for _GTK_EDGE_CONSTRAINTS failed.",
            self.xcb.get_property(
                false,
                self.x_window,
                state.atoms._GTK_EDGE_CONSTRAINTS,
                xproto::AtomEnum::CARDINAL,
                0,
                4,
            ),
        )?;

        if reply.value_len != 0 {
            if let Ok(bytes) = reply.value[0..4].try_into() {
                let atom = u32::from_ne_bytes(bytes);
                let edge_constraints = EdgeConstraints::from_atom(atom);
                state.edge_constraints.replace(edge_constraints);
            } else {
                log::error!("Failed to parse GTK_EDGE_CONSTRAINTS");
            }
        }

        Ok(())
    }

    fn set_wm_properties(
        &self,
        mut state: std::cell::RefMut<X11WindowState>,
    ) -> anyhow::Result<()> {
        let reply = get_reply(
            || "X11 GetProperty for _NET_WM_STATE failed.",
            self.xcb.get_property(
                false,
                self.x_window,
                state.atoms._NET_WM_STATE,
                xproto::AtomEnum::ATOM,
                0,
                u32::MAX,
            ),
        )?;

        let atoms = reply
            .value
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));

        let was_active = state.active;
        state.active = false;
        state.fullscreen = false;
        state.maximized_vertical = false;
        state.maximized_horizontal = false;
        state.hidden = false;

        for atom in atoms {
            if atom == state.atoms._NET_WM_STATE_FOCUSED {
                state.active = true;
            } else if atom == state.atoms._NET_WM_STATE_FULLSCREEN {
                state.fullscreen = true;
            } else if atom == state.atoms._NET_WM_STATE_MAXIMIZED_VERT {
                state.maximized_vertical = true;
            } else if atom == state.atoms._NET_WM_STATE_MAXIMIZED_HORZ {
                state.maximized_horizontal = true;
            } else if atom == state.atoms._NET_WM_STATE_HIDDEN {
                state.hidden = true;
            }
        }

        // The urgency hint has no withdrawal signal of its own; ICCCM leaves that to
        // the client, and focus is the conventional means for the user to zero it.
        if state.active && !was_active {
            set_wm_hints_urgency(&self.xcb, self.x_window, false);
        }

        Ok(())
    }

    pub fn add_child(&self, child: xproto::Window) {
        let mut state = self.state.borrow_mut();
        state.children.insert(child);
    }

    pub fn is_blocked(&self) -> bool {
        let state = self.state.borrow();
        !state.children.is_empty()
    }

    pub fn close(&self) {
        let state = self.state.borrow();
        let client = state.client.clone();
        #[allow(clippy::mutable_key_type)]
        let children = state.children.clone();
        drop(state);

        if let Some(client) = client.get_client() {
            for child in children {
                if let Some(child_window) = client.get_window(child) {
                    child_window.close();
                }
            }
        }
        self.close_popups();

        let mut callbacks = self.callbacks.borrow_mut();
        if let Some(fun) = callbacks.close.take() {
            fun()
        }
    }

    /// Closes every popup anchored to this window through the normal close path, so each
    /// `on_close` runs once: the callback is taken when it runs.
    pub(crate) fn close_popups(&self) {
        let state = self.state.borrow();
        let client = state.client.clone();
        let popups: Vec<xproto::Window> = state.popups.iter().copied().collect();
        drop(state);

        if let Some(client) = client.get_client() {
            for popup in popups {
                if let Some(popup_window) = client.get_window(popup) {
                    popup_window.close();
                }
            }
        }
    }

    /// Places every popup anchored to this window again, after this window moved or resized.
    pub(crate) fn reposition_popups(&self) {
        let state = self.state.borrow();
        if state.popups.is_empty() {
            return;
        }
        let client = state.client.clone();
        let popups: Vec<xproto::Window> = state.popups.iter().copied().collect();
        drop(state);

        if let Some(client) = client.get_client() {
            for popup in popups {
                if let Some(popup_window) = client.get_window(popup) {
                    popup_window.place_popup();
                }
            }
        }
    }

    /// Moves and sizes this popup to where its options put it against its parent now. Writes
    /// the size hints first: they pin the size, so a resize the hints do not allow would be
    /// refused.
    fn place_popup(&self) {
        let state = self.state.borrow();
        let Some(popup) = state.popup.as_ref() else {
            return;
        };
        let Some(bounds) = place_popup_on_screen(
            &self.xcb,
            &state.atoms,
            state.x_screen_index,
            popup.parent.x_window,
            &popup.options,
            popup.size,
            state.scale_factor,
        )
        .log_err() else {
            return;
        };
        drop(state);

        check_reply(
            || "X11 change of WM_SIZE_HINTS for a popup failed.",
            popup_size_hints(bounds).set_normal_hints(&*self.xcb, self.x_window),
        )
        .log_err();
        check_reply(
            || format!("X11 ConfigureWindow for a popup failed. bounds: {bounds:?}"),
            self.xcb.configure_window(
                self.x_window,
                &xproto::ConfigureWindowAux::new()
                    .x(bounds.origin.x.0)
                    .y(bounds.origin.y.0)
                    .width(bounds.size.width.0 as u32)
                    .height(bounds.size.height.0 as u32),
            ),
        )
        .log_err();
        xcb_flush(&self.xcb);
    }

    fn popup_timestamp(&self, time: xproto::Timestamp) {
        {
            let mut state = self.state.borrow_mut();
            let Some(popup) = state.popup.as_mut() else {
                return;
            };
            if !matches!(popup.focus, PopupFocus::AwaitingTimestamp) {
                return;
            }
            popup.focus = PopupFocus::Pending(time);
        }
        self.focus_popup_if_pending();
    }

    /// Gives a popup that asked for focus the keyboard, once it has both a timestamp and a
    /// viewable window. Called on the timestamp's PropertyNotify, on MapNotify and on
    /// VisibilityNotify, since which comes last depends on the window manager.
    ///
    /// The X input focus goes to the parent, or stays with it, and the client sends the
    /// parent's keystrokes here. _NET_ACTIVE_WINDOW (source 1, an application) is the request
    /// a window manager with focus stealing prevention weighs; SetInputFocus with the same
    /// fresh timestamp follows, for window managers without EWMH or that refused, and for a
    /// native child window of the parent that holds the keyboard.
    pub(crate) fn focus_popup_if_pending(&self) {
        let state = self.state.borrow();
        let Some(popup) = state.popup.as_ref() else {
            return;
        };
        let PopupFocus::Pending(time) = popup.focus else {
            return;
        };
        let parent = popup.parent.x_window;
        let root = state.x_root_window;
        let active_window = state.atoms._NET_ACTIVE_WINDOW;
        let client = state.client.get_client();
        drop(state);

        let viewable = get_reply(
            || "X11 GetWindowAttributes for a popup failed.",
            self.xcb.get_window_attributes(self.x_window),
        )
        .log_err()
        .is_some_and(|attributes| attributes.map_state == xproto::MapState::VIEWABLE);
        if !viewable {
            return;
        }

        if let Some(popup) = self.state.borrow().popup.as_ref() {
            popup.parent.state.borrow_mut().keyboard_popup = Some(self.x_window);
        }
        if let Some(client) = client {
            client.give_keyboard_to_popup(self, parent);
        }

        let message = ClientMessageEvent::new(32, parent, active_window, [1, time, 0, 0, 0]);
        check_reply(
            || "X11 SendEvent of _NET_ACTIVE_WINDOW for a popup failed.",
            self.xcb.send_event(
                false,
                root,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )
        .log_err();
        // Unchecked: if the window stopped being viewable since the query, the BadMatch is
        // an error event the event loop drops, and the window manager's answer stands.
        self.xcb
            .set_input_focus(xproto::InputFocus::POINTER_ROOT, parent, time)
            .log_err();
        xcb_flush(&self.xcb);

        if let Some(popup) = self.state.borrow_mut().popup.as_mut() {
            popup.focus = PopupFocus::Done;
        }
    }

    pub fn refresh(&self, mut request_frame_options: RequestFrameOptions) {
        let callback = self.callbacks.borrow_mut().request_frame.take();
        if let Some(mut fun) = callback {
            // Expose events can present a frame before the refresh timer runs,
            // so every frame request must rebuild stale atlas references after recovery.
            request_frame_options.force_render |=
                std::mem::take(&mut self.state.borrow_mut().force_render_after_recovery);
            fun(request_frame_options);
            self.callbacks.borrow_mut().request_frame = Some(fun);
        }
    }

    pub fn handle_input(&self, input: PlatformInput) {
        if self.is_blocked() {
            return;
        }
        let callback = self.callbacks.borrow_mut().input.take();
        if let Some(mut fun) = callback {
            let result = fun(input.clone());
            self.callbacks.borrow_mut().input = Some(fun);
            if !result.propagate {
                return;
            }
        }
        if let PlatformInput::KeyDown(event) = input {
            // only allow shift modifier when inserting text
            if event.keystroke.modifiers.is_subset_of(&Modifiers::shift()) {
                let mut state = self.state.borrow_mut();
                if let Some(mut input_handler) = state.input_handler.take() {
                    if let Some(key_char) = &event.keystroke.key_char {
                        drop(state);
                        input_handler.replace_text_in_range(None, key_char);
                        state = self.state.borrow_mut();
                    }
                    state.input_handler = Some(input_handler);
                }
            }
        }
    }

    pub fn handle_ime_commit(&self, text: String) {
        if self.is_blocked() {
            return;
        }
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            input_handler.replace_text_in_range(None, &text);
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn handle_ime_preedit(&self, text: String) {
        if self.is_blocked() {
            return;
        }
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            input_handler.replace_and_mark_text_in_range(None, &text, None);
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn handle_ime_unmark(&self) {
        if self.is_blocked() {
            return;
        }
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            input_handler.unmark_text();
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn handle_ime_delete(&self) {
        if self.is_blocked() {
            return;
        }
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            if let Some(marked) = input_handler.marked_text_range() {
                input_handler.replace_text_in_range(Some(marked), "");
            }
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn get_ime_area(&self) -> Option<Bounds<ScaledPixels>> {
        let (scale_factor, input_handler) = {
            let mut state = self.state.borrow_mut();
            (state.scale_factor, state.input_handler.take())
        };
        let mut input_handler = input_handler?;
        let bounds = input_handler
            .selected_text_range(true)
            .and_then(|selection| input_handler.bounds_for_range(selection.range));
        self.state.borrow_mut().input_handler = Some(input_handler);
        let offset = self.offset_in_popup_parent();
        bounds.map(|b| b.scale(scale_factor) + offset.scale(scale_factor))
    }

    /// Where this window's origin is in its popup parent, or zero for a window that is not an
    /// anchored popup. The input method's client window is the parent, which holds the X input
    /// focus for the popup, so a caret position in the popup is given to it from the parent's
    /// origin.
    pub(crate) fn offset_in_popup_parent(&self) -> Point<Pixels> {
        let state = self.state.borrow();
        let Some(popup) = state.popup.as_ref() else {
            return Point::default();
        };
        let scale_factor = state.scale_factor;
        let parent = popup.parent.x_window;
        drop(state);
        get_reply(
            || "X11 TranslateCoordinates for a popup in its parent failed.",
            self.xcb.translate_coordinates(self.x_window, parent, 0, 0),
        )
        .log_err()
        .map(|origin| {
            Point::new(
                px(f32::from(origin.dst_x) / scale_factor),
                px(f32::from(origin.dst_y) / scale_factor),
            )
        })
        .unwrap_or_default()
    }

    pub fn set_bounds(&self, bounds: Bounds<i32>) -> anyhow::Result<()> {
        let (is_resize, content_size, scale_factor) = {
            let mut state = self.state.borrow_mut();
            let bounds = bounds.map(|f| px(f as f32 / state.scale_factor));

            let is_resize = bounds.size.width != state.bounds.size.width
                || bounds.size.height != state.bounds.size.height;

            // If it's a resize event (only width/height changed), we ignore `bounds.origin`
            // because it contains wrong values.
            if is_resize {
                state.bounds.size = bounds.size;
            } else {
                state.bounds = bounds;
            }

            let gpu_size = query_render_extent(&self.xcb, self.x_window)?;
            if let Some(renderer) = &mut state.renderer {
                renderer.update_drawable_size(gpu_size);
            }
            let result = (is_resize, state.content_size(), state.scale_factor);
            if let Some(value) = state.last_sync_counter.take() {
                check_reply(
                    || "X11 sync SetCounter failed.",
                    sync::set_counter(&self.xcb, state.counter_id, value),
                )?;
            }
            result
        };

        let mut callbacks = self.callbacks.borrow_mut();
        if let Some(ref mut fun) = callbacks.resize {
            fun(content_size, scale_factor)
        }

        if !is_resize && let Some(ref mut fun) = callbacks.moved {
            fun();
        }

        Ok(())
    }

    /// Tells GPUI whether this window has the keyboard, itself or through one of its own
    /// popups, given the window that has it now, if that changed since it was last told. Under
    /// its popup a window stays the window in front, as it does under a menu.
    pub(crate) fn report_activation(&self, focused: Option<xproto::Window>) {
        let active = {
            let mut state = self.state.borrow_mut();
            let active = focused
                .is_some_and(|focused| focused == self.x_window || state.popups.contains(&focused));
            if state.reported_active.replace(active) == Some(active) {
                return;
            }
            active
        };
        self.set_active(active);
    }

    /// The popup of this window that has the keyboard, if one does.
    pub(crate) fn keyboard_popup(&self) -> Option<xproto::Window> {
        self.state.borrow().keyboard_popup
    }

    /// Takes the keyboard back from this window's popup, returning the popup that had it.
    pub(crate) fn take_keyboard_popup(&self) -> Option<xproto::Window> {
        self.state.borrow_mut().keyboard_popup.take()
    }

    /// The parent of this window, if it is an anchored popup.
    pub(crate) fn popup_parent(&self) -> Option<X11WindowStatePtr> {
        self.state
            .borrow()
            .popup
            .as_ref()
            .map(|popup| popup.parent.clone())
    }

    pub fn set_active(&self, focus: bool) {
        let callback = self.callbacks.borrow_mut().active_status_change.take();
        if let Some(mut fun) = callback {
            fun(focus);
            self.callbacks.borrow_mut().active_status_change = Some(fun);
        }
        if let Some(adapter) = self.state.borrow_mut().accesskit_adapter.as_mut() {
            adapter.update_window_focus_state(focus);
        }
    }

    pub fn set_hovered(&self, focus: bool) {
        let callback = self.callbacks.borrow_mut().hovered_status_change.take();
        if let Some(mut fun) = callback {
            fun(focus);
            self.callbacks.borrow_mut().hovered_status_change = Some(fun);
        }
    }

    pub fn set_visibility(&self, visibility: WindowVisibility) {
        if std::mem::replace(&mut self.state.borrow_mut().visibility, visibility) == visibility {
            return;
        }
        let callback = self.callbacks.borrow_mut().visibility_change.take();
        if let Some(mut fun) = callback {
            fun(visibility);
            self.callbacks.borrow_mut().visibility_change = Some(fun);
        }
    }

    pub fn set_appearance(&mut self, appearance: WindowAppearance) {
        let mut state = self.state.borrow_mut();
        state.appearance = appearance;
        state.update_transparency();
        state.appearance = appearance;
        drop(state);
        let callback = self.callbacks.borrow_mut().appearance_changed.take();
        if let Some(mut fun) = callback {
            fun();
            self.callbacks.borrow_mut().appearance_changed = Some(fun);
        }
    }

    pub fn set_button_layout(&self) {
        let callback = self.callbacks.borrow_mut().button_layout_changed.take();
        if let Some(mut fun) = callback {
            fun();
            self.callbacks.borrow_mut().button_layout_changed = Some(fun);
        }
    }
}

impl PlatformWindow for X11Window {
    fn bounds(&self) -> Bounds<Pixels> {
        self.0.state.borrow().bounds
    }

    fn is_maximized(&self) -> bool {
        let state = self.0.state.borrow();

        // A maximized window that gets minimized will still retain its maximized state.
        !state.hidden && state.maximized_vertical && state.maximized_horizontal
    }

    fn window_bounds(&self) -> WindowBounds {
        let state = self.0.state.borrow();
        if self.is_maximized() {
            WindowBounds::Maximized(state.bounds)
        } else {
            WindowBounds::Windowed(state.bounds)
        }
    }

    fn inner_window_bounds(&self) -> WindowBounds {
        let state = self.0.state.borrow();
        if self.is_maximized() {
            WindowBounds::Maximized(state.bounds)
        } else {
            let mut bounds = state.bounds;
            let [left, right, top, bottom] = state.last_insets;

            let [left, right, top, bottom] = [
                px((left as f32) / state.scale_factor),
                px((right as f32) / state.scale_factor),
                px((top as f32) / state.scale_factor),
                px((bottom as f32) / state.scale_factor),
            ];

            bounds.origin.x += left;
            bounds.origin.y += top;
            bounds.size.width -= left + right;
            bounds.size.height -= top + bottom;

            WindowBounds::Windowed(bounds)
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        // After the wgpu migration, X11WindowState::content_size() returns logical pixels
        // (bounds.size is already divided by scale_factor in set_bounds), so no further
        // division is needed here. This matches the Wayland implementation.
        self.0.state.borrow().content_size()
    }

    fn resize(&mut self, size: Size<Pixels>) {
        // A popup keeps its anchor: it is placed again for the new size rather than grown from
        // its top-left corner.
        let is_popup = {
            let mut state = self.0.state.borrow_mut();
            if let Some(popup) = state.popup.as_mut() {
                popup.size = size;
            }
            state.popup.is_some()
        };
        if is_popup {
            self.0.place_popup();
            return;
        }

        let state = self.0.state.borrow();
        let size = size.to_device_pixels(state.scale_factor);
        let width = size.width.0 as u32;
        let height = size.height.0 as u32;

        check_reply(
            || {
                format!(
                    "X11 ConfigureWindow failed. width: {}, height: {}",
                    width, height
                )
            },
            self.0.xcb.configure_window(
                self.0.x_window,
                &xproto::ConfigureWindowAux::new()
                    .width(width)
                    .height(height),
            ),
        )
        .log_err();
        xcb_flush(&self.0.xcb);
    }

    fn set_popup_anchor(&self, anchor_rect: Bounds<Pixels>) {
        {
            let mut state = self.0.state.borrow_mut();
            let Some(popup) = state.popup.as_mut() else {
                return;
            };
            popup.options.anchor_rect = anchor_rect;
        }
        self.0.place_popup();
    }

    fn scale_factor(&self) -> f32 {
        self.0.state.borrow().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        self.0.state.borrow().appearance
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.0.state.borrow().display.clone())
    }

    fn mouse_position(&self) -> Point<Pixels> {
        get_reply(
            || "X11 QueryPointer failed.",
            self.0.xcb.query_pointer(self.0.x_window),
        )
        .log_err()
        .map_or(Point::new(Pixels::ZERO, Pixels::ZERO), |reply| {
            let scale_factor = self.0.state.borrow().scale_factor;
            Point::new(
                px(reply.win_x as f32 / scale_factor),
                px(reply.win_y as f32 / scale_factor),
            )
        })
    }

    fn modifiers(&self) -> Modifiers {
        self.0
            .state
            .borrow()
            .client
            .0
            .upgrade()
            .map(|ref_cell| ref_cell.borrow().modifiers)
            .unwrap_or_default()
    }

    fn capslock(&self) -> gpui::Capslock {
        self.0
            .state
            .borrow()
            .client
            .0
            .upgrade()
            .map(|ref_cell| ref_cell.borrow().capslock)
            .unwrap_or_default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.state.borrow_mut().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.state.borrow_mut().input_handler.take()
    }

    fn show_character_palette(&self) {
        let Some(client) = self.0.state.borrow().client.get_client() else {
            return;
        };
        let executor = client
            .0
            .borrow()
            .common
            .borrow()
            .background_executor
            .clone();
        crate::linux::character_palette::show_character_palette(executor, None);
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self, token: Option<&str>) -> bool {
        if token.is_some() {
            return false;
        }
        let data = [1, xproto::Time::CURRENT_TIME.into(), 0, 0, 0];
        let message = xproto::ClientMessageEvent::new(
            32,
            self.0.x_window,
            self.0.state.borrow().atoms._NET_ACTIVE_WINDOW,
            data,
        );
        self.0
            .xcb
            .send_event(
                false,
                self.0.state.borrow().x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            )
            .log_err();
        xcb_flush(&self.0.xcb);
        true
    }

    fn request_attention(&self) {
        if self.is_active() {
            return;
        }

        set_wm_hints_urgency(&self.0.xcb, self.0.x_window, true);
    }

    fn is_active(&self) -> bool {
        let state = self.0.state.borrow();
        if state.active {
            return true;
        }
        // Or one of its popups has the keyboard.
        let Some(client) = state.client.get_client() else {
            return false;
        };
        let popups: Vec<xproto::Window> = state.popups.iter().copied().collect();
        drop(state);
        popups.into_iter().any(|popup| {
            client
                .get_window(popup)
                .is_some_and(|popup| popup.state.borrow().active)
        })
    }

    fn visibility(&self) -> WindowVisibility {
        self.0.state.borrow().visibility
    }

    fn is_hovered(&self) -> bool {
        self.0.state.borrow().hovered
    }

    fn set_title(&mut self, title: &str) {
        check_reply(
            || "X11 ChangeProperty8 on WM_NAME failed.",
            self.0.xcb.change_property8(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                xproto::AtomEnum::WM_NAME,
                xproto::AtomEnum::STRING,
                title.as_bytes(),
            ),
        )
        .log_err();

        check_reply(
            || "X11 ChangeProperty8 on _NET_WM_NAME failed.",
            self.0.xcb.change_property8(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                self.0.state.borrow().atoms._NET_WM_NAME,
                self.0.state.borrow().atoms.UTF8_STRING,
                title.as_bytes(),
            ),
        )
        .log_err();
        xcb_flush(&self.0.xcb);
    }

    fn set_app_id(&mut self, app_id: &str) {
        let mut data = Vec::with_capacity(app_id.len() * 2 + 2);
        data.extend(app_id.bytes()); // instance https://unix.stackexchange.com/a/494170
        data.push(b'\0');
        data.extend(app_id.bytes()); // class
        data.push(b'\0');

        check_reply(
            || "X11 ChangeProperty8 for WM_CLASS failed.",
            self.0.xcb.change_property8(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                xproto::AtomEnum::WM_CLASS,
                xproto::AtomEnum::STRING,
                &data,
            ),
        )
        .log_err();
    }

    fn map_window(&mut self) -> anyhow::Result<()> {
        if self.0.state.borrow().visible {
            check_reply(
                || "X11 MapWindow failed.",
                self.0.xcb.map_window(self.0.x_window),
            )?;

            let state = self.0.state.borrow();
            if let Some(popup) = state.popup.as_ref()
                && matches!(popup.focus, PopupFocus::AwaitingTimestamp)
            {
                // The PropertyNotify this causes carries a server timestamp later than the input
                // that opened the popup, which may have gone to another client (a native child
                // window), so this process has no recent event time of its own to focus with.
                check_reply(
                    || "X11 ChangeProperty for a popup's focus timestamp failed.",
                    self.0.xcb.change_property8(
                        xproto::PropMode::APPEND,
                        self.0.x_window,
                        state.atoms._GPUI_TIMESTAMP,
                        xproto::AtomEnum::STRING,
                        &[],
                    ),
                )?;
                xcb_flush(&self.0.xcb);
            }
        }
        Ok(())
    }

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        let mut state = self.0.state.borrow_mut();
        state.background_appearance = background_appearance;
        state.update_transparency();
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        self.0.state.borrow().background_appearance
    }

    fn set_visible(&self, visible: bool) {
        let mut state = self.0.state.borrow_mut();
        if state.visible == visible {
            return;
        }
        state.visible = visible;
        drop(state);

        let result = if visible {
            self.0.xcb.map_window(self.0.x_window)
        } else {
            self.0.xcb.unmap_window(self.0.x_window)
        };
        check_reply(
            || {
                if visible {
                    "X11 MapWindow failed."
                } else {
                    "X11 UnmapWindow failed."
                }
            },
            result,
        )
        .log_err();
        xcb_flush(&self.0.xcb);
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        self.0
            .state
            .borrow()
            .client
            .0
            .upgrade()
            .map(|ref_cell| {
                let state = ref_cell.borrow();
                state
                    .gpu_context
                    .borrow()
                    .as_ref()
                    .is_some_and(|ctx| ctx.supports_dual_source_blending())
            })
            .unwrap_or_default()
    }

    fn minimize(&self) {
        let state = self.0.state.borrow();
        const WINDOW_ICONIC_STATE: u32 = 3;
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms.WM_CHANGE_STATE,
            [WINDOW_ICONIC_STATE, 0, 0, 0, 0],
        );
        check_reply(
            || "X11 SendEvent to minimize window failed.",
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )
        .log_err();
    }

    fn zoom(&self) {
        let state = self.0.state.borrow();
        self.set_wm_hints(
            || "X11 SendEvent to maximize a window failed.",
            WmHintPropertyState::Toggle,
            state.atoms._NET_WM_STATE_MAXIMIZED_VERT,
            state.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
        )
        .log_err();
    }

    fn toggle_fullscreen(&self) {
        let state = self.0.state.borrow();
        self.set_wm_hints(
            || "X11 SendEvent to fullscreen a window failed.",
            WmHintPropertyState::Toggle,
            state.atoms._NET_WM_STATE_FULLSCREEN,
            xproto::AtomEnum::NONE.into(),
        )
        .log_err();
    }

    fn is_fullscreen(&self) -> bool {
        self.0.state.borrow().fullscreen
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.callbacks.borrow_mut().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> gpui::DispatchEventResult>) {
        self.0.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.0.callbacks.borrow_mut().visibility_change = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().hovered_status_change = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().moved = Some(callback);
    }

    // A GPUI display on X11 is an X screen, which spans every monitor, and a
    // window never changes screen.
    fn on_display_changed(&self, _callback: Box<dyn FnMut()>) {}

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn on_button_layout_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().button_layout_changed = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        let mut inner = self.0.state.borrow_mut();
        let inner = &mut *inner;
        let Some(renderer) = &mut inner.renderer else {
            return;
        };

        if renderer.device_lost() {
            let raw_window = RawWindow {
                connection: as_raw_xcb_connection::AsRawXcbConnection::as_raw_xcb_connection(
                    &*self.0.xcb,
                ) as *mut _,
                screen_id: inner.x_screen_index,
                window_id: self.0.x_window,
                visual_id: inner.visual_id,
            };
            match renderer.recover(&raw_window) {
                Ok(()) => {}
                Err(err) => {
                    log::warn!("GPU recovery failed, will retry on next frame: {err}");
                }
            }

            inner.force_render_after_recovery = true;
            return;
        }

        renderer.draw(scene);

        if renderer.needs_redraw() {
            inner.force_render_after_recovery = true;
        }
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.0
            .state
            .borrow()
            .renderer
            .as_ref()
            .expect("the renderer is only taken when the window is dropped")
            .sprite_atlas()
            .clone()
    }

    fn show_window_menu(&self, position: Point<Pixels>) {
        let state = self.0.state.borrow();

        check_reply(
            || "X11 UngrabPointer failed.",
            self.0.xcb.ungrab_pointer(x11rb::CURRENT_TIME),
        )
        .log_err();

        let Some(coords) = self.get_root_position(position).log_err() else {
            return;
        };
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms._GTK_SHOW_WINDOW_MENU,
            [
                XINPUT_ALL_DEVICE_GROUPS as u32,
                coords.dst_x as u32,
                coords.dst_y as u32,
                0,
                0,
            ],
        );
        check_reply(
            || "X11 SendEvent to show window menu failed.",
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )
        .log_err();
    }

    fn start_window_move(&self) {
        const MOVERESIZE_MOVE: u32 = 8;
        self.send_moveresize(MOVERESIZE_MOVE).log_err();
    }

    fn start_window_resize(&self, edge: ResizeEdge) {
        self.send_moveresize(resize_edge_to_moveresize(edge))
            .log_err();
    }

    fn window_decorations(&self) -> gpui::Decorations {
        let state = self.0.state.borrow();

        // Client window decorations require compositor support
        if !state.client_side_decorations_supported {
            return Decorations::Server;
        }

        match state.decorations {
            WindowDecorations::Server => Decorations::Server,
            WindowDecorations::Client => {
                let tiling = if state.fullscreen {
                    Tiling::tiled()
                } else if let Some(edge_constraints) = &state.edge_constraints {
                    edge_constraints.to_tiling()
                } else {
                    // https://source.chromium.org/chromium/chromium/src/+/main:ui/ozone/platform/x11/x11_window.cc;l=2519;drc=1f14cc876cc5bf899d13284a12c451498219bb2d
                    Tiling {
                        top: state.maximized_vertical,
                        bottom: state.maximized_vertical,
                        left: state.maximized_horizontal,
                        right: state.maximized_horizontal,
                    }
                };
                Decorations::Client { tiling }
            }
        }
    }

    fn set_client_inset(&self, inset: Pixels) {
        let mut state = self.0.state.borrow_mut();

        let dp = (f32::from(inset) * state.scale_factor) as u32;

        let insets = if state.fullscreen {
            [0, 0, 0, 0]
        } else if let Some(edge_constraints) = &state.edge_constraints {
            let left = if edge_constraints.left_tiled { 0 } else { dp };
            let top = if edge_constraints.top_tiled { 0 } else { dp };
            let right = if edge_constraints.right_tiled { 0 } else { dp };
            let bottom = if edge_constraints.bottom_tiled { 0 } else { dp };

            [left, right, top, bottom]
        } else {
            let (left, right) = if state.maximized_horizontal {
                (0, 0)
            } else {
                (dp, dp)
            };
            let (top, bottom) = if state.maximized_vertical {
                (0, 0)
            } else {
                (dp, dp)
            };
            [left, right, top, bottom]
        };

        if state.last_insets != insets {
            state.last_insets = insets;

            check_reply(
                || "X11 ChangeProperty for _GTK_FRAME_EXTENTS failed.",
                self.0.xcb.change_property(
                    xproto::PropMode::REPLACE,
                    self.0.x_window,
                    state.atoms._GTK_FRAME_EXTENTS,
                    xproto::AtomEnum::CARDINAL,
                    size_of::<u32>() as u8 * 8,
                    4,
                    bytemuck::cast_slice::<u32, u8>(&insets),
                ),
            )
            .log_err();
        }
    }

    fn request_decorations(&self, mut decorations: gpui::WindowDecorations) {
        let mut state = self.0.state.borrow_mut();

        if matches!(decorations, gpui::WindowDecorations::Client)
            && !state.client_side_decorations_supported
        {
            log::info!(
                "x11: no compositor present, falling back to server-side window decorations"
            );
            decorations = gpui::WindowDecorations::Server;
        }

        // https://github.com/rust-windowing/winit/blob/master/src/platform_impl/linux/x11/util/hint.rs#L53-L87
        let hints_data: [u32; 5] = match decorations {
            // A popup has no frame whichever is asked for: a title bar would move it off
            // its anchor and offer to move it by hand.
            _ if state.popup.is_some() => MOTIF_NO_DECORATIONS,
            WindowDecorations::Server => [1 << 1, 0, 1, 0, 0],
            WindowDecorations::Client => [1 << 1, 0, 0, 0, 0],
        };

        let success = check_reply(
            || "X11 ChangeProperty for _MOTIF_WM_HINTS failed.",
            self.0.xcb.change_property(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                state.atoms._MOTIF_WM_HINTS,
                state.atoms._MOTIF_WM_HINTS,
                size_of::<u32>() as u8 * 8,
                5,
                bytemuck::cast_slice::<u32, u8>(&hints_data),
            ),
        )
        .log_err();

        let Some(()) = success else {
            return;
        };

        match decorations {
            WindowDecorations::Server => {
                state.decorations = WindowDecorations::Server;
                state.update_transparency();
            }
            WindowDecorations::Client => {
                state.decorations = WindowDecorations::Client;
                state.update_transparency();
            }
        }

        drop(state);
        let mut callbacks = self.0.callbacks.borrow_mut();
        if let Some(appearance_changed) = callbacks.appearance_changed.as_mut() {
            appearance_changed();
        }
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        let state = self.0.state.borrow();
        let client = state.client.clone();
        drop(state);
        client.update_ime_position(bounds + self.0.offset_in_popup_parent());
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        Some(self.0.state.borrow().renderer.as_ref()?.gpu_specs())
    }

    fn gpu_context(&self) -> Option<Box<dyn std::any::Any>> {
        let state = self.0.state.borrow();
        let (device, queue) = state.renderer.as_ref()?.gpu_context();
        Some(Box::new((device, queue)))
    }

    fn gpu_context_info(&self) -> Option<Box<dyn std::any::Any>> {
        self.0
            .state
            .borrow()
            .renderer
            .as_ref()?
            .gpu_context_info()
            .map(|context| Box::new(context) as Box<dyn std::any::Any>)
    }

    fn gpu_device_lost(&self) -> Option<bool> {
        // Only loads an atomic flag — safe even mid-recovery, when
        // `gpu_context` would panic on the torn-down resources.
        self.0
            .state
            .borrow()
            .renderer
            .as_ref()
            .map(WgpuRenderer::device_lost)
    }

    fn play_system_bell(&self) {
        // Volume 0% means don't increase or decrease from system volume
        let _ = self.0.xcb.bell(0);
    }

    fn a11y_init(&self, callbacks: gpui::A11yCallbacks) {
        let activation_handler = TrivialActivationHandler {
            callback: callbacks.activation,
        };
        let action_handler = TrivialActionHandler(callbacks.action);
        let deactivation_handler = TrivialDeactivationHandler {
            callback: callbacks.deactivation,
        };

        let adapter =
            accesskit_unix::Adapter::new(activation_handler, action_handler, deactivation_handler);

        self.0.state.borrow_mut().accesskit_adapter = Some(adapter);
    }

    fn a11y_tree_update(&self, tree_update: accesskit::TreeUpdate) {
        let mut state = self.0.state.borrow_mut();
        if let Some(adapter) = state.accesskit_adapter.as_mut() {
            adapter.update_if_active(|| tree_update);
        }
    }

    fn a11y_update_window_bounds(&self) {
        let mut state = self.0.state.borrow_mut();
        let scale = state.scale_factor;
        let bounds = state.bounds;
        let [left, right, top, bottom] = state.last_insets;

        let x = f32::from(bounds.origin.x);
        let y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);

        let outer = accesskit::Rect {
            x0: (x * scale) as f64,
            y0: (y * scale) as f64,
            x1: ((x + width) * scale) as f64,
            y1: ((y + height) * scale) as f64,
        };

        let inner = accesskit::Rect {
            x0: (x * scale) as f64 + left as f64,
            y0: (y * scale) as f64 + top as f64,
            x1: ((x + width) * scale) as f64 - right as f64,
            y1: ((y + height) * scale) as f64 - bottom as f64,
        };

        if let Some(adapter) = state.accesskit_adapter.as_mut() {
            adapter.set_root_window_bounds(outer, inner);
        }
    }
}

struct TrivialActivationHandler {
    callback: Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send + 'static>,
}

impl accesskit::ActivationHandler for TrivialActivationHandler {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        (self.callback)()
    }
}

struct TrivialActionHandler(Box<dyn Fn(accesskit::ActionRequest) + Send + 'static>);

impl accesskit::ActionHandler for TrivialActionHandler {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        (self.0)(request);
    }
}

struct TrivialDeactivationHandler {
    callback: Box<dyn Fn() + Send + 'static>,
}

impl accesskit::DeactivationHandler for TrivialDeactivationHandler {
    fn deactivate_accessibility(&mut self) {
        (self.callback)();
    }
}
