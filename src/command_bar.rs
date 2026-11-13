//! The command palette in a window of its own, above a host window that
//! embeds a live native view.
//!
//! A native view — a web view, a video player — is composited over the
//! host's GPUI window, so the in-window [`command_palette`] is hidden
//! wherever the view is. The command bar is the same palette panel in a
//! small window owned by the host's: it stays above that window and no
//! other application's, follows it, and leaves the native view showing and
//! running underneath. The panel, the query, the keys and the ranking are
//! the palette's own; only the window around them is new.
//!
//! It needs a GPUI backend that implements
//! [`WindowKind::AnchoredPopup`]: the
//! vendored GPUI-CE in this repository does on macOS, Windows and X11. On a
//! backend that does not, [`open_command_bar`] returns the backend's error
//! rather than falling back to a palette the native view would cover.
//!
//! ## Lifecycle
//!
//! - Opening puts the keyboard in the query field. Opening `id` again
//!   while it is open brings the open one back to the front with the
//!   keyboard, rather than making a second.
//! - Escape dismisses it. Choosing a command closes it first.
//! - It is dismissed when it loses activation: a click in its parent or in
//!   any other window, or a switch to another application. The click goes
//!   on to whatever it landed on.
//! - The host window keeps looking and behaving active while the bar has
//!   the keyboard, as it does under a menu: the bar is its child, not a
//!   window in front of it.
//! - It arrives as the in-window palette does: the same fade and short
//!   drift, in a window that is the panel and nothing more.
//! - With [`CommandBarOptions::dim`], a scrim darkens the host window's
//!   content, native view included, while the bar is open, as the
//!   in-window palette's does; a press on it dismisses the bar and goes no
//!   further. There is no scrim where one cannot be shown: on an X11 screen
//!   with no compositing manager, and under Wayland.
//! - It follows its parent when the parent moves, is re-anchored when the
//!   parent is resized, and closes with the parent.
//! - `on_done` runs at most once, after the window is gone and outside any
//!   update of the bar, on the host, with the host's window — exactly once
//!   while that window is open.
//! - The keyboard goes back to the focus handle that had it in the host
//!   when the bar opened, and the host window is activated again, only when
//!   the bar closed from inside itself: Escape or a choice. A bar dismissed
//!   because another application came forward leaves that application in
//!   front.
//!
//! Sessions belong to the host that opened them, in its
//! [`ControlState`]: two host windows can each have a
//! bar open, and one host's bar never closes another's.
//!
//! [`command_palette`]: crate::command_palette

use std::rc::Rc;

use std::cell::Cell;
use std::time::{Duration, Instant};

use gpui::popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, MouseButton, Pixels, Render,
    SharedString, Size, Subscription, Window, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, point, prelude::*, px, size,
};

use crate::controls::WidgetContext;
use crate::easing::{ease_out_cubic, progress};
use crate::lighting;
use crate::overlay::{
    self, Activate, Command, PALETTE_WIDTH, command_results, palette_fill, palette_panel,
    palette_panel_height,
};
use crate::state::{COMBO_REVEAL, ComboId, ControlHost, ControlState};
use crate::text_input::{InputStyle, TextInput};
use crate::{Palette, keyboard};

/// How a command bar ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandBarOutcome {
    /// A command was chosen: its `Command::id`.
    Chosen(SharedString),
    /// The bar closed without a choice.
    Dismissed(Dismissal),
}

/// Why a command bar closed without a choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dismissal {
    /// Escape.
    Escape,
    /// The bar lost activation: a click outside it, or another application
    /// coming forward.
    Deactivated,
    /// The host closed it with [`close_command_bar`].
    Closed,
    /// A press on the dimming scrim, as on the in-window palette's.
    Scrim,
    /// The window went without the bar asking: the platform closed it, or
    /// its parent closed.
    WindowClosed,
}

impl Dismissal {
    /// Whether the bar closed from inside itself, the one case where the
    /// host window should take the keyboard back. Otherwise something else
    /// has it now, or is about to.
    fn closed_from_inside(self) -> bool {
        matches!(
            self,
            Dismissal::Escape | Dismissal::Closed | Dismissal::Scrim
        )
    }
}

/// How a command bar is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandBarOptions {
    /// Darken the host window's content, native view and all, while the bar
    /// is open, the way the in-window palette's scrim does. Off by default:
    /// a bar over live content is often there to act on what is showing.
    /// Ignored where the desktop cannot blend one window over another (an
    /// X11 screen with no compositing manager): the scrim would be opaque.
    /// Ignored under Wayland too, where the bar holds the keyboard through a
    /// popup grab and a compositor dismisses a grabbing popup that has
    /// another popup of the same window beside it.
    pub dim: bool,
}

type Done<V> = Box<dyn FnOnce(&mut V, CommandBarOutcome, &mut Window, &mut Context<V>)>;

/// One open command bar, kept in the host's [`ControlState`].
pub(crate) struct Session {
    id: ComboId,
    window: WindowHandle<CommandBarWindow>,
    /// Re-anchors the bar, and resizes the scrim, when the host window is
    /// resized. Dropped with the session.
    _resized: Subscription,
}

/// Where a bar sits on a host window whose content area is `viewport`:
/// centred across it, its top a little above a sixth of the way down — where
/// the in-window palette sits.
fn anchor_rect(viewport: gpui::Size<gpui::Pixels>) -> Bounds<gpui::Pixels> {
    Bounds::new(
        point(px(0.0), viewport.height * 0.16),
        size(viewport.width, px(0.0)),
    )
}

/// The bar's width on a host window `viewport` wide: the palette's, or nine
/// tenths of a narrower window.
fn bar_width(viewport: gpui::Size<gpui::Pixels>) -> f32 {
    PALETTE_WIDTH
        .min(f32::from(viewport.width) * 0.9)
        .max(160.0)
}

/// Opens command bar `id` over the host's window, or brings the open one
/// back with the keyboard. `commands` are filtered and ranked as in
/// [`command_palette`](crate::command_palette); `on_done` hears how it
/// ended — see the [module documentation](self) for when.
///
/// # Errors
///
/// Fails when the GPUI backend cannot open a window owned by another.
#[allow(clippy::too_many_arguments)]
pub fn open_command_bar<V: ControlHost>(
    this: &mut V,
    id: ComboId,
    commands: Vec<Command>,
    palette: Palette,
    options: CommandBarOptions,
    window: &mut Window,
    cx: &mut Context<V>,
    on_done: impl FnOnce(&mut V, CommandBarOutcome, &mut Window, &mut Context<V>) + 'static,
) -> gpui::Result<()> {
    if let Some(open) = command_bar(this.control_state(), id) {
        let reopened = open.update(cx, |bar, bar_window, cx| {
            if bar.ended {
                return false;
            }
            bar_window.activate();
            let focus = bar.query.read(cx).focus_handle.clone();
            bar_window.focus(&focus, cx);
            true
        });
        if matches!(reopened, Ok(true)) {
            return Ok(());
        }
        forget(this.control_state_mut(), open);
    }

    let parent = window.window_handle();
    let host = cx.entity().downgrade();
    let return_focus = window.focused(cx);
    let viewport = window.viewport_size();
    let width = bar_width(viewport);
    let time_scale = this.control_state().time_scale();
    let reveal_duration = this.control_state().scaled(COMBO_REVEAL);
    let display_id = window.display(cx).map(|display| display.id());
    // Opened before the bar so it is ordered under it. It never takes the
    // keyboard, so a press on it leaves the bar active long enough to hear
    // the press.
    let bar_slot: Rc<Cell<Option<WindowHandle<CommandBarWindow>>>> = Rc::default();
    let scrim = if options.dim && can_dim(cx) {
        let slot = bar_slot.clone();
        Some(cx.open_window(
            popup_window(
                parent,
                Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(0.0))),
                PopupAnchor::TopLeft,
                PopupGravity::BottomRight,
                PopupConstraintAdjustment::empty(),
                viewport,
                false,
                display_id,
            ),
            |_, cx| cx.new(|_| Scrim::new(palette, slot, reveal_duration)),
        )?)
    } else {
        None
    };
    let finish: Finish = {
        let slot = bar_slot.clone();
        let on_done: std::cell::RefCell<Option<Done<V>>> =
            std::cell::RefCell::new(Some(Box::new(on_done)));
        Rc::new(move |outcome: CommandBarOutcome, cx: &mut App| {
            let Some(on_done) = on_done.borrow_mut().take() else {
                return;
            };
            let host = host.clone();
            let return_focus = return_focus.clone();
            let slot = slot.clone();
            // Deferred: the bar's window is gone by now, and the host runs
            // outside whatever update closed it.
            cx.defer(move |cx| {
                // Here rather than before deferring: a press on the scrim
                // ends the bar from inside the scrim's own update, where
                // removing the scrim's window would be refused and the tint
                // would stay.
                if let Some(scrim) = scrim {
                    let _ = scrim.update(cx, |_, window, _| window.remove_window());
                }
                // An unsuccessful open has no session or completion to report.
                let Some(closed) = slot.get() else {
                    return;
                };
                let _ = parent.update(cx, |_, window, cx| {
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    let inside = match &outcome {
                        CommandBarOutcome::Chosen(_) => true,
                        CommandBarOutcome::Dismissed(why) => why.closed_from_inside(),
                    };
                    host.update(cx, |host, cx| {
                        forget(host.control_state_mut(), closed);
                        // A replacement bar may already own the keyboard by the
                        // time this deferred completion reaches the host.
                        if inside && host.control_state().command_bars.is_empty() {
                            if !window.is_window_active() {
                                window.activate();
                            }
                            if let Some(focus) = &return_focus {
                                window.focus(focus, cx);
                            }
                        }
                        on_done(host, outcome, window, cx);
                        cx.notify();
                    });
                });
            });
        })
    };

    let bar_options = popup_window(
        parent,
        anchor_rect(viewport),
        PopupAnchor::Top,
        PopupGravity::Bottom,
        PopupConstraintAdjustment::SLIDE_X
            | PopupConstraintAdjustment::SLIDE_Y
            | PopupConstraintAdjustment::RESIZE_Y,
        size(px(width), px(120.0)),
        true,
        display_id,
    );
    let bar = match cx.open_window(bar_options, |bar_window, cx| {
        cx.new(|cx| {
            CommandBarWindow::new(
                id,
                commands,
                palette,
                finish.clone(),
                time_scale,
                bar_window,
                cx,
            )
        })
    }) {
        Ok(bar) => bar,
        Err(error) => {
            if let Some(scrim) = scrim {
                let _ = scrim.update(cx, |_, window, _| window.remove_window());
            }
            return Err(error);
        }
    };
    bar_slot.set(Some(bar));

    let resized = cx.observe_window_bounds(window, move |_, window, cx| {
        let viewport = window.viewport_size();
        if let Some(scrim) = scrim {
            let _ = scrim.update(cx, |_, scrim_window, _| scrim_window.resize(viewport));
        }
        let _ = bar.update(cx, |bar, bar_window, cx| {
            bar_window.set_popup_anchor(anchor_rect(viewport));
            bar.width = bar_width(viewport);
            cx.notify();
        });
    });
    this.control_state_mut().command_bars.push(Session {
        id,
        window: bar,
        _resized: resized,
    });
    Ok(())
}

/// Whether a scrim window can go under the bar on this desktop: see
/// [`CommandBarOptions::dim`].
fn can_dim(cx: &App) -> bool {
    cx.windows_are_composited() && cx.compositor_name() != "Wayland"
}

/// The options for a window anchored to `parent`: undecorated, fixed, its
/// background clear so what it draws can fade in.
#[allow(clippy::too_many_arguments)]
fn popup_window(
    parent: AnyWindowHandle,
    anchor_rect: Bounds<Pixels>,
    anchor: PopupAnchor,
    gravity: PopupGravity,
    constraint_adjustment: PopupConstraintAdjustment,
    window_size: Size<Pixels>,
    focus: bool,
    display_id: Option<gpui::DisplayId>,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.0), px(0.0)),
            window_size,
        ))),
        titlebar: None,
        focus,
        show: true,
        kind: WindowKind::AnchoredPopup(PopupOptions {
            parent,
            anchor_rect,
            anchor,
            gravity,
            constraint_adjustment,
            offset: point(px(0.0), px(0.0)),
            // Wayland gives a popup the keyboard only with a grab; the other backends ignore
            // this and go by `focus`.
            grab: focus,
        }),
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id,
        // The panel draws its own rounded fill; the scrim its own tint.
        #[cfg(target_os = "macos")]
        macos_window_background: gpui::MacosWindowBackground::Transparent,
        #[cfg(target_os = "windows")]
        windows_window_background: gpui::WindowsWindowBackground::Transparent,
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        linux_window_background: gpui::LinuxWindowBackground::Transparent,
        ..Default::default()
    }
}

/// The scrim a dimmed command bar lays over its host window.
struct Scrim {
    palette: Palette,
    opened_at: Instant,
    reveal_duration: Duration,
    bar: Rc<Cell<Option<WindowHandle<CommandBarWindow>>>>,
}

impl Scrim {
    fn new(
        palette: Palette,
        bar: Rc<Cell<Option<WindowHandle<CommandBarWindow>>>>,
        reveal_duration: Duration,
    ) -> Self {
        Self {
            palette,
            opened_at: Instant::now(),
            reveal_duration,
            bar,
        }
    }
}

impl Render for Scrim {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reveal = ease_out_cubic(progress(self.opened_at, self.reveal_duration));
        if reveal < 1.0 {
            window.request_animation_frame();
        }
        let scrim = self.palette.scrim();
        div()
            .id("vampir-command-bar-scrim")
            .size_full()
            .bg(crate::color::with_alpha(scrim, scrim.alpha * reveal))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|scrim, _, _, cx| {
                    if let Some(bar) = scrim.bar.get() {
                        let _ = bar.update(cx, |bar, bar_window, cx| {
                            bar.end(
                                CommandBarOutcome::Dismissed(Dismissal::Scrim),
                                bar_window,
                                cx,
                            );
                        });
                    }
                }),
            )
    }
}

/// Closes command bar `id` if the host has it open; its `on_done` hears
/// [`Dismissal::Closed`].
pub fn close_command_bar<V: ControlHost>(this: &V, id: ComboId, cx: &mut Context<V>) {
    if let Some(bar) = command_bar(this.control_state(), id) {
        let _ = bar.update(cx, |bar, bar_window, cx| {
            bar.end(
                CommandBarOutcome::Dismissed(Dismissal::Closed),
                bar_window,
                cx,
            );
        });
    }
}

/// Replaces the commands of open command bar `id`, for a host whose list
/// changes while the bar is up. The highlight stays on the command it was
/// on, by id, as in the in-window palette.
pub fn set_command_bar_commands<V: ControlHost>(
    this: &V,
    id: ComboId,
    commands: Vec<Command>,
    cx: &mut Context<V>,
) {
    if let Some(bar) = command_bar(this.control_state(), id) {
        let _ = bar.update(cx, |bar, _, cx| {
            bar.commands = commands;
            bar.matches.invalidate();
            cx.notify();
        });
    }
}

/// Whether the host has command bar `id` open.
pub fn is_command_bar_open(state: &ControlState, id: ComboId) -> bool {
    command_bar(state, id).is_some()
}

fn command_bar(state: &ControlState, id: ComboId) -> Option<WindowHandle<CommandBarWindow>> {
    state
        .command_bars
        .iter()
        .find(|session| session.id == id)
        .map(|session| session.window)
}

fn forget(state: &mut ControlState, closed: WindowHandle<CommandBarWindow>) {
    state
        .command_bars
        .retain(|session| session.window.window_id() != closed.window_id());
}

type Finish = Rc<dyn Fn(CommandBarOutcome, &mut App)>;

// Matching depends on the query and the private command list, not on animation
// frames or window bounds. Share the cached rows while render updates the host.
#[derive(Default)]
struct CommandMatches {
    query: Option<SharedString>,
    rows: Rc<[overlay::SearchResult]>,
}

impl CommandMatches {
    fn results(
        &mut self,
        query: &SharedString,
        commands: &[Command],
    ) -> Rc<[overlay::SearchResult]> {
        if self.query.as_ref() != Some(query) {
            self.rows = command_results(query, commands).into();
            self.query = Some(query.clone());
        }
        self.rows.clone()
    }

    fn invalidate(&mut self) {
        self.query = None;
    }
}

/// The root view of a command bar's window: the palette panel, filling it.
pub struct CommandBarWindow {
    id: ComboId,
    controls: ControlState,
    query: Entity<TextInput>,
    commands: Vec<Command>,
    matches: CommandMatches,
    palette: Palette,
    width: f32,
    finish: Finish,
    /// Set once the window has been active, so the activation it starts
    /// without is not mistaken for having lost it.
    was_active: bool,
    ended: bool,
    _activation: Subscription,
}

impl ControlHost for CommandBarWindow {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }

    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }
}

impl CommandBarWindow {
    fn new(
        id: ComboId,
        commands: Vec<Command>,
        palette: Palette,
        finish: Finish,
        time_scale: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(
                cx,
                "Type a command",
                false,
                InputStyle::from_palette(palette, 13.0),
            )
        });
        let mut controls = ControlState::default();
        if let Some(scale) = time_scale {
            controls.set_time_scale(scale);
        }
        // The in-window palette's own open: the highlight on the first row,
        // the list at its top, the keyboard in the field.
        controls.open_palette(id, &query, window, cx);
        let activation = cx.observe_window_activation(window, |bar, window, cx| {
            if window.is_window_active() {
                bar.was_active = true;
            } else if bar.was_active {
                bar.end(
                    CommandBarOutcome::Dismissed(Dismissal::Deactivated),
                    window,
                    cx,
                );
            }
        });
        // The window can go without the bar asking: the platform closes it,
        // or its parent closes and takes it along. The host still hears.
        cx.on_release(|bar, cx| {
            if !bar.ended {
                bar.ended = true;
                (bar.finish)(CommandBarOutcome::Dismissed(Dismissal::WindowClosed), cx);
            }
        })
        .detach();
        Self {
            id,
            controls,
            query,
            commands,
            matches: CommandMatches::default(),
            palette,
            width: f32::from(window.viewport_size().width),
            finish,
            was_active: window.is_window_active(),
            ended: false,
            _activation: activation,
        }
    }

    /// Closes the window, then tells the host. Only the first ending counts.
    fn end(&mut self, outcome: CommandBarOutcome, window: &mut Window, cx: &mut Context<Self>) {
        if self.ended {
            return;
        }
        self.ended = true;
        window.remove_window();
        (self.finish)(outcome, cx);
    }
}

impl Render for CommandBarWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let palette = self.palette;
        let text = self.query.read(cx).shared_text();
        let matches = self.matches.results(&text, &self.commands);
        let on_activate: Activate<Self> = Rc::new(move |bar, picked, window, cx| {
            bar.end(CommandBarOutcome::Chosen(picked), window, cx);
        });
        let list = overlay::result_list(
            id,
            &text,
            "",
            &matches,
            overlay::PALETTE_RESULT_MAX_HEIGHT,
            WidgetContext {
                palette,
                view: self,
                cx,
            },
            on_activate.clone(),
        );
        // The window is the panel and nothing more: anything around it
        // would catch the pointer meant for the host underneath.
        let wanted = size(px(self.width), px(palette_panel_height(&list)));
        if (window.viewport_size().height - wanted.height).abs() > px(0.5)
            || (window.viewport_size().width - wanted.width).abs() > px(0.5)
        {
            window.resize(wanted);
        }
        let panel = palette_panel(
            id,
            &self.query,
            list,
            "No matching commands",
            WidgetContext {
                palette,
                view: self,
                cx,
            },
            window,
            on_activate,
            Rc::new(
                |bar: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    bar.end(CommandBarOutcome::Dismissed(Dismissal::Escape), window, cx);
                },
            ),
        );
        // The in-window palette's arrival: a fade and a short drift down
        // into place. The drift starts above the window's top edge, where
        // the panel is still all but transparent.
        let reveal = overlay::palette_opened_at(&self.controls, id).map_or(1.0, |opened_at| {
            ease_out_cubic(progress(opened_at, self.controls.scaled(COMBO_REVEAL)))
        });
        let fill = palette_fill(palette);
        let mut root = div().id("vampir-command-bar");
        if !cx.windows_are_composited() {
            // Nothing blends this window over the host, so what it leaves clear — the
            // corners outside the panel's radius, the panel while it fades — would be black.
            root = root.bg(fill);
        }
        let element = keyboard::handle_keys(root, self, cx)
            .size_full()
            .text_color(palette.text_primary)
            .font(crate::ui_font_face())
            .child(
                panel
                    .size_full()
                    .relative()
                    .top(px(-6.0 * (1.0 - reveal)))
                    .opacity(reveal)
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(lighting::rim(fill, palette.is_dark)),
            );
        if self.controls.animating() || reveal < 1.0 {
            window.request_animation_frame();
        }
        element
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_completion_preserves_the_replacement_and_its_resize_subscription() {
        let old = WindowHandle::new(gpui::WindowId::from(1));
        let replacement = WindowHandle::new(gpui::WindowId::from(2));
        let cancelled = Rc::new(Cell::new(false));
        let mut state = ControlState::default();
        state.command_bars.push(Session {
            id: "commands",
            window: replacement,
            _resized: Subscription::new({
                let cancelled = cancelled.clone();
                move || cancelled.set(true)
            }),
        });
        forget(&mut state, old);
        assert_eq!(
            command_bar(&state, "commands").unwrap().window_id(),
            replacement.window_id()
        );
        assert!(!cancelled.get());
        forget(&mut state, replacement);
        assert!(!is_command_bar_open(&state, "commands"));
        assert!(cancelled.get());
    }

    #[test]
    fn unchanged_queries_reuse_rows_and_command_changes_invalidate_them() {
        let mut matches = CommandMatches::default();
        let commands = [
            Command::new("open", "Open file"),
            Command::new("save", "Save file"),
        ];
        let first = matches.results(&"file".into(), &commands);
        let repeated = matches.results(&"file".into(), &commands);
        assert!(Rc::ptr_eq(&first, &repeated));
        let changed = matches.results(&"save".into(), &commands);
        assert!(!Rc::ptr_eq(&first, &changed));
        assert_eq!(changed[0].id.as_ref(), "save");
        matches.invalidate();
        let replaced = matches.results(&"save".into(), &[Command::new("new", "Save all")]);
        assert_eq!(replaced[0].id.as_ref(), "new");
        assert!(!Rc::ptr_eq(&changed, &replaced));
    }

    #[test]
    #[ignore = "timing benchmark; run explicitly without concurrent tests"]
    fn command_bar_cache_benchmark() {
        use crate::menu::ownership_bench::measure;
        use std::hint::black_box;
        let commands: Vec<_> = (0..1000)
            .map(|index| Command::new(format!("command-{index}"), format!("Open settings {index}")))
            .collect();
        for query in ["", "ops"] {
            let query = SharedString::from(query);
            let mut cache = CommandMatches::default();
            cache.results(&query, &commands);
            measure(&format!("unchanged_query_{query:?}_before"), || {
                black_box(command_results(black_box(&query), black_box(&commands)));
            });
            measure(&format!("unchanged_query_{query:?}_after"), || {
                black_box(cache.results(black_box(&query), black_box(&commands)));
            });
        }
    }

    #[test]
    fn the_bar_sits_where_the_in_window_palette_does() {
        let rect = anchor_rect(size(px(1000.0), px(600.0)));
        assert_eq!(rect.origin, point(px(0.0), px(96.0)));
        assert_eq!(rect.size.width, px(1000.0));
    }

    #[test]
    fn a_narrow_host_gets_a_narrower_bar_but_never_a_sliver() {
        assert_eq!(bar_width(size(px(1200.0), px(800.0))), PALETTE_WIDTH);
        assert_eq!(bar_width(size(px(400.0), px(800.0))), 360.0);
        assert_eq!(bar_width(size(px(100.0), px(800.0))), 160.0);
    }

    #[test]
    fn only_a_bar_closed_from_inside_takes_the_host_window_back() {
        assert!(Dismissal::Escape.closed_from_inside());
        assert!(Dismissal::Closed.closed_from_inside());
        assert!(!Dismissal::Deactivated.closed_from_inside());
        assert!(!Dismissal::WindowClosed.closed_from_inside());
    }
}
