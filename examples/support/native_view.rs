//! A real native child view embedded in a GPUI window, for the gallery's
//! Native view page: what a web view or a video player is to a host, and
//! what the toolkit's command bar has to appear above without hiding it.
//!
//! - macOS: an editable `NSTextView` in a scroll view, beside a spinning
//!   `NSProgressIndicator`, added to the window's content view.
//! - Windows: a multi-line `EDIT` control, a child `HWND` of GPUI's.
//! - X11: a child window of GPUI's whose colour cycles twice a second, from
//!   a connection and a thread of its own.
//!
//! Each is composited over everything GPUI draws where it is: that is the
//! whole problem it stands for.

use gpui::{Bounds, Pixels, Window};

pub struct NativeView(imp::View);

impl NativeView {
    /// Embeds a native view in `window`, hidden until it is given bounds.
    /// `None` where the platform's window handle is not one this knows.
    pub fn new(window: &Window) -> Option<Self> {
        imp::View::new(window).map(NativeView)
    }

    /// Puts the view at `bounds`, in the window's content coordinates, and
    /// shows it; `None` hides it, for a page that is not showing.
    pub fn place(&self, bounds: Option<Bounds<Pixels>>, window: &Window) {
        self.0.place(bounds, window);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use gpui::{Bounds, Pixels, Window};
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSProgressIndicator, NSProgressIndicatorStyle, NSTextView,
        NSView, NSWindowOrderingMode,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub struct View {
        container: Retained<NSView>,
        content: Retained<NSView>,
    }

    impl View {
        pub fn new(window: &Window) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            let RawWindowHandle::AppKit(handle) =
                HasWindowHandle::window_handle(window).ok()?.as_raw()
            else {
                return None;
            };
            // SAFETY: GPUI hands out its own live NSView, on the main thread.
            let gpui_view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
            let content = gpui_view.window()?.contentView()?;
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0));
            let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
            container.setHidden(true);

            let text = NSTextView::scrollableTextView(mtm);
            text.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            if let Some(document) = text.documentView()
                && let Ok(document) = document.downcast::<NSTextView>()
            {
                document.setString(&NSString::from_str(
                    "A native NSTextView, composited over the GPUI window.\n\n\
                     Type here, then press ⌘J: the command bar opens above this \
                     view in a window of its own, and this view stays visible and \
                     live underneath — the spinner keeps turning, and what you \
                     typed is still here when the bar closes.",
                ));
            }
            container.addSubview(&text);

            let spinner = NSProgressIndicator::initWithFrame(
                NSProgressIndicator::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(32.0, 32.0)),
            );
            spinner.setStyle(NSProgressIndicatorStyle::Spinning);
            spinner.setIndeterminate(true);
            spinner.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewMinXMargin
                    | NSAutoresizingMaskOptions::ViewMinYMargin,
            );
            unsafe { spinner.startAnimation(None) };
            container.addSubview(&spinner);

            content.addSubview_positioned_relativeTo(&container, NSWindowOrderingMode::Above, None);
            let view = View { container, content };
            view.layout_children(NSSize::new(10.0, 10.0));
            Some(view)
        }

        fn layout_children(&self, size: NSSize) {
            let subviews = self.container.subviews();
            if let Some(text) = subviews.firstObject() {
                text.setFrame(NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new((size.width - 48.0).max(0.0), size.height),
                ));
            }
            if let Some(spinner) = subviews.lastObject() {
                spinner.setFrame(NSRect::new(
                    NSPoint::new(size.width - 40.0, size.height - 40.0),
                    NSSize::new(32.0, 32.0),
                ));
            }
        }

        pub fn place(&self, bounds: Option<Bounds<Pixels>>, _window: &Window) {
            let Some(bounds) = bounds else {
                self.container.setHidden(true);
                return;
            };
            // AppKit's content view counts up from the bottom; GPUI's
            // coordinates count down from the top.
            let height = self.content.frame().size.height;
            let size = NSSize::new(
                f32::from(bounds.size.width) as f64,
                f32::from(bounds.size.height) as f64,
            );
            let frame = NSRect::new(
                NSPoint::new(
                    f32::from(bounds.origin.x) as f64,
                    height - f32::from(bounds.origin.y) as f64 - size.height,
                ),
                size,
            );
            if self.container.frame() != frame {
                self.container.setFrame(frame);
                self.layout_children(size);
            }
            self.container.setHidden(false);
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::cell::Cell;

    use gpui::{Bounds, Pixels, Window};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL, HFONT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, ES_AUTOVSCROLL, ES_MULTILINE, LWA_ALPHA, SW_HIDE, SW_SHOWNA,
        SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetLayeredWindowAttributes, SetWindowPos,
        ShowWindow, WINDOW_STYLE, WM_SETFONT, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_LAYERED,
        WS_VSCROLL,
    };
    use windows::core::w;

    pub struct View {
        hwnd: HWND,
        /// The control's font and the scale it was made for. Without one an
        /// `EDIT` draws in the stock system font, which does not follow the
        /// monitor's scale: tiny at 200%.
        font: Cell<Option<(HFONT, f32)>>,
    }

    impl View {
        /// Gives the control a font sized for `scale`, if it does not have one.
        fn match_font(&self, scale: f32) {
            if self
                .font
                .get()
                .is_some_and(|(_, made_for)| made_for == scale)
            {
                return;
            }
            // 9 points, the size of the desktop's own text, at this scale.
            let height = -(12.0 * scale).round() as i32;
            let font = unsafe {
                CreateFontW(
                    height,
                    0,
                    0,
                    0,
                    FW_NORMAL.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    Default::default(),
                    Default::default(),
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                )
            };
            if font.is_invalid() {
                return;
            }
            unsafe {
                SendMessageW(
                    self.hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            if let Some((old, _)) = self.font.replace(Some((font, scale))) {
                let _ = unsafe { DeleteObject(old.into()) };
            }
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            if let Some((font, _)) = self.font.take() {
                let _ = unsafe { DeleteObject(font.into()) };
            }
        }
    }

    impl View {
        pub fn new(window: &Window) -> Option<Self> {
            let RawWindowHandle::Win32(handle) =
                HasWindowHandle::window_handle(window).ok()?.as_raw()
            else {
                return None;
            };
            let parent = HWND(handle.hwnd.get() as *mut _);
            let instance = unsafe { GetModuleHandleW(None) }.ok()?;
            // Layered, because GPUI's window is `WS_EX_NOREDIRECTIONBITMAP`: it
            // draws only through DirectComposition, so a plain child has no
            // surface for the desktop compositor to show. A layered child
            // (Windows 8 and later) gets one of its own.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_CLIENTEDGE | WS_EX_LAYERED,
                    w!("EDIT"),
                    w!("A native EDIT control, a child HWND of the GPUI window.\r\n\r\nType here: the keys are this control's own while it has the keyboard. Then open the command bar with the button above, or click outside this control and press Ctrl+J: it opens above this control in a window of its own, and the control stays visible and live underneath."),
                    WS_CHILD
                        | WS_VSCROLL
                        | WINDOW_STYLE(ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32),
                    0,
                    0,
                    10,
                    10,
                    Some(parent),
                    None,
                    Some(instance.into()),
                    None,
                )
            }
            .ok()?;
            // A layered window shows nothing until it is given its opacity.
            unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) }.ok()?;
            let view = View {
                hwnd,
                font: Cell::new(None),
            };
            view.match_font(window.scale_factor());
            Some(view)
        }

        pub fn place(&self, bounds: Option<Bounds<Pixels>>, window: &Window) {
            let Some(bounds) = bounds else {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                return;
            };
            let scale = window.scale_factor();
            // The window may have moved to a monitor of another scale.
            self.match_font(scale);
            let device = |pixels: Pixels| (f32::from(pixels) * scale).round() as i32;
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    device(bounds.origin.x),
                    device(bounds.origin.y),
                    device(bounds.size.width),
                    device(bounds.size.height),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod imp {
    use std::time::Duration;

    use gpui::{Bounds, Pixels, Window};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
        WindowClass,
    };
    use x11rb::rust_connection::RustConnection;

    pub struct View {
        connection: RustConnection,
        window: u32,
    }

    impl View {
        pub fn new(window: &Window) -> Option<Self> {
            // X11 only: under Wayland there is no child window to make.
            let RawWindowHandle::Xcb(handle) =
                HasWindowHandle::window_handle(window).ok()?.as_raw()
            else {
                return None;
            };
            let parent = handle.window.get();
            let (connection, _) = x11rb::connect(None).ok()?;
            let child = connection.generate_id().ok()?;
            connection
                .create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    child,
                    parent,
                    0,
                    0,
                    10,
                    10,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    x11rb::COPY_FROM_PARENT,
                    &CreateWindowAux::new().background_pixel(0x2d6cdf),
                )
                .ok()?;
            connection.flush().ok()?;
            // Live content: its colour cycles on its own connection, so it
            // keeps changing whatever GPUI is doing.
            std::thread::spawn(move || {
                let Ok((connection, _)) = x11rb::connect(None) else {
                    return;
                };
                let colours = [0x2d6cdf, 0x2fa36b, 0xd9822b, 0x9b59b6];
                for colour in colours.iter().cycle() {
                    let changed = connection
                        .change_window_attributes(
                            child,
                            &ChangeWindowAttributesAux::new().background_pixel(*colour),
                        )
                        .and_then(|_| connection.clear_area(true, child, 0, 0, 0, 0))
                        .and_then(|_| connection.flush());
                    if changed.is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
            Some(View {
                connection,
                window: child,
            })
        }

        pub fn place(&self, bounds: Option<Bounds<Pixels>>, window: &Window) {
            let Some(bounds) = bounds else {
                let _ = self.connection.unmap_window(self.window);
                let _ = self.connection.flush();
                return;
            };
            let scale = window.scale_factor();
            let device = |pixels: Pixels| (f32::from(pixels) * scale).round() as i32;
            let _ = self.connection.configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .x(device(bounds.origin.x))
                    .y(device(bounds.origin.y))
                    .width(device(bounds.size.width).max(1) as u32)
                    .height(device(bounds.size.height).max(1) as u32),
            );
            let _ = self.connection.map_window(self.window);
            let _ = self.connection.flush();
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd"
)))]
mod imp {
    use gpui::{Bounds, Pixels, Window};

    pub struct View;

    impl View {
        pub fn new(_: &Window) -> Option<Self> {
            None
        }

        pub fn place(&self, _: Option<Bounds<Pixels>>, _: &Window) {}
    }
}
