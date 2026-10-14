//! Hints and passing messages in a little pop-up window of their own, for
//! a host whose window has a native view over it: a tooltip drawn inside
//! the window would be hidden wherever the view is, or cut off at its
//! edge.
//!
//! The window never takes the keyboard, sits beside or below what it names
//! rather than under the pointer, and is kept on the screen. [`crate::hinted`]
//! uses [`attach`] when the host has asked for floating hints; a host
//! reaches for [`toast`] itself, for a message that has nowhere else to go.

use std::{cell::RefCell, time::Duration};

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Div, FontWeight, InteractiveElement,
    MouseButton, ParentElement, Pixels, Point, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions, canvas, div, point, px, size,
};

use crate::lighting;
use crate::overlay::{Hint, HintSide};
use crate::palette::Palette;
use crate::state::{ControlHost, Tag};
use crate::typography::{text_width, ui_font};

/// How long the pointer rests on something before its hint shows.
const DELAY: Duration = Duration::from_millis(450);
/// How long a passing message stays.
const TOAST: Duration = Duration::from_millis(2800);
/// Between a hint and what it names.
const GAP: f32 = 6.0;
const SIDE_GAP: f32 = 8.0;
/// Kept clear of the screen's edges.
const SCREEN_MARGIN: f32 = 4.0;
const TEXT_SIZE: f32 = 11.5;
const PAD_X: f32 = 8.0;
const PAD_Y: f32 = 4.0;
const LINE_HEIGHT: f32 = 16.0;
/// Room around the panel for its shadow, inside the transparent window.
const SHADOW: f32 = 24.0;

thread_local! {
    static HINT: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
    /// A passing message's own window, so a hint doesn't take it away.
    static TOAST_WINDOW: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
}

/// Gives `element` a hint in a window of its own, shown after the pointer
/// has rested on it for a moment and taken away when the pointer leaves,
/// the element is pressed, or [`dismiss`] is called. The hint is keyed by
/// the element's id, so two elements never fight over one window.
pub fn attach<V: ControlHost>(
    element: Stateful<Div>,
    hint: impl Into<Hint>,
    side: HintSide,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let mut element = element;
    let hint = hint.into();
    let text = match &hint.shortcut {
        Some(shortcut) => format!("{}   {shortcut}", hint.text),
        None => hint.text.to_string(),
    };
    // By host too: each window draws its own.
    let key = match element.interactivity().element_id.clone() {
        Some(id) => Tag::new((cx.entity_id(), side, id)),
        None => Tag::new((cx.entity_id(), side, text.as_str())),
    };
    let weak = cx.entity().downgrade();
    element
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            hover(this, key, text.clone(), side, *hovered, window, cx);
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| dismiss(this, cx)),
        )
        .child(
            // Records where the hinted element is drawn, for the window to
            // sit beside.
            canvas(
                move |bounds, _, cx| {
                    if let Some(host) = weak.upgrade() {
                        host.update(cx, |host, _| {
                            host.control_state_mut().measure(key, bounds);
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

/// The pointer came onto (or left) something with a hint: shows it after
/// a moment's rest, or takes it away.
fn hover<V: ControlHost>(
    view: &mut V,
    key: Tag,
    text: String,
    side: HintSide,
    hovered: bool,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let state = view.control_state_mut();
    if !hovered {
        if state.hint_key == Some(key) {
            state.hint_key = None;
            state.hint_timer = None;
            hide(cx);
        }
        return;
    }
    state.hint_key = Some(key);
    state.hint_timer = Some(cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(DELAY).await;
        let _ = this.update_in(cx, |host, window, cx| {
            let state = host.control_state();
            if state.hint_key != Some(key) {
                return;
            }
            let palette = state.palette();
            if let Some(bounds) = state.measured(key) {
                show_in(&HINT, text.into(), bounds, side, palette, window, cx);
            }
        });
    }));
}

/// A click, a key or leaving the window: no hint. For whatever the host
/// knows takes the pointer away without a hover-out, such as a hinted
/// element being removed from under it.
pub fn dismiss<V: ControlHost>(view: &mut V, cx: &mut App) {
    let state = view.control_state_mut();
    state.hint_key = None;
    state.hint_timer = None;
    hide(cx);
}

/// Hides the hint, if one is showing.
pub fn hide(cx: &mut App) {
    close(&HINT, cx);
}

/// Shows `text` under `anchor` (window-relative, as GPUI lays out) in the
/// host's window for a few seconds: for what an action says when the
/// place such messages sit isn't showing.
pub fn toast<V: ControlHost>(
    view: &mut V,
    text: impl Into<SharedString>,
    anchor: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let palette = view.control_state().palette();
    show_in(
        &TOAST_WINDOW,
        text.into(),
        anchor,
        HintSide::Below,
        palette,
        window,
        cx,
    );
    view.control_state_mut().toast_timer = Some(cx.spawn(async move |_, cx| {
        cx.background_executor().timer(TOAST).await;
        cx.update(hide_toast);
    }));
}

/// Takes the passing message away.
pub fn hide_toast(cx: &mut App) {
    close(&TOAST_WINDOW, cx);
}

fn close(slot: &'static std::thread::LocalKey<RefCell<Option<AnyWindowHandle>>>, cx: &mut App) {
    if let Some(handle) = slot.with(|slot| slot.borrow_mut().take()) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// The window's origin for `anchor` in a host window at `frame`, both in
/// the global space GPUI's window bounds use: centred below the anchor, or
/// beside it to the right, then kept inside `screen`.
fn place(
    frame: Point<f32>,
    anchor: Bounds<f32>,
    (width, height): (f32, f32),
    side: HintSide,
    screen: Option<Bounds<f32>>,
) -> Point<f32> {
    let (mut x, mut y) = match side {
        HintSide::Below => (
            frame.x + anchor.origin.x + anchor.size.width / 2.0 - width / 2.0,
            frame.y + anchor.origin.y + anchor.size.height + GAP,
        ),
        HintSide::Right => (
            frame.x + anchor.origin.x + anchor.size.width + SIDE_GAP,
            frame.y + anchor.origin.y + anchor.size.height / 2.0 - height / 2.0,
        ),
    };
    if let Some(screen) = screen {
        x = x.clamp(
            screen.origin.x + SCREEN_MARGIN,
            screen.origin.x + screen.size.width - width - SCREEN_MARGIN,
        );
        y = y.min(screen.origin.y + screen.size.height - height - SCREEN_MARGIN);
    }
    point(x, y)
}

/// What the window shows.
struct Panel {
    text: SharedString,
    palette: Palette,
}

impl Render for Panel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let palette = self.palette;
        let dark = palette.is_dark;
        // The same surface a tooltip drawn in the window would have.
        let fill = if dark {
            palette.soft_fill
        } else {
            palette.field_surface
        };
        div()
            .size_full()
            .p(px(SHADOW))
            .flex()
            .items_start()
            .justify_start()
            .font_family(ui_font())
            .child(
                div()
                    .px(px(PAD_X))
                    .py(px(PAD_Y))
                    .h(px(LINE_HEIGHT + 2.0 * PAD_Y))
                    .flex()
                    .items_center()
                    .rounded(px(5.0))
                    .bg(fill)
                    .border_1()
                    .border_color(lighting::rim(fill, dark))
                    .shadow(lighting::panel(dark))
                    .text_size(px(TEXT_SIZE))
                    .text_color(palette.text_primary)
                    .whitespace_nowrap()
                    .child(self.text.clone()),
            )
    }
}

#[allow(clippy::too_many_arguments)]
fn show_in(
    slot: &'static std::thread::LocalKey<RefCell<Option<AnyWindowHandle>>>,
    text: SharedString,
    anchor: Bounds<Pixels>,
    side: HintSide,
    palette: Palette,
    window: &mut Window,
    cx: &mut App,
) {
    // A window cannot be moved once open, so each showing is a new one.
    close(slot, cx);
    let width = text_width(cx, &text, TEXT_SIZE, FontWeight::NORMAL) + 2.0 * PAD_X + 2.0;
    let height = LINE_HEIGHT + 2.0 * PAD_Y + 2.0;
    let frame = crate::popup::content_origin(window);
    let anchor = Bounds::new(
        point(f32::from(anchor.origin.x), f32::from(anchor.origin.y)),
        size(f32::from(anchor.size.width), f32::from(anchor.size.height)),
    );
    let display = window.display(cx);
    let screen = display.as_ref().map(|display| {
        let bounds = display.bounds();
        Bounds::new(
            point(f32::from(bounds.origin.x), f32::from(bounds.origin.y)),
            size(f32::from(bounds.size.width), f32::from(bounds.size.height)),
        )
    });
    let origin = place(
        point(f32::from(frame.x), f32::from(frame.y)),
        anchor,
        (width, height),
        side,
        screen,
    );
    let options = WindowOptions {
        display_id: display.map(|display| display.id()),
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(origin.x - SHADOW), px(origin.y - SHADOW)),
            size(px(width + 2.0 * SHADOW), px(height + 2.0 * SHADOW)),
        ))),
        kind: WindowKind::PopUp,
        focus: false,
        window_background: WindowBackgroundAppearance::Transparent,
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
        is_movable: false,
        ..Default::default()
    };
    if let Ok(handle) = cx.open_window(options, |_, cx| cx.new(|_| Panel { text, palette })) {
        slot.with(|slot| *slot.borrow_mut() = Some(handle.into()));
        // The new window must not take the keyboard from the host's: a
        // platform that gives it to the newest window gets it back, so the
        // host's next click is a click and not the one that focuses it.
        window.activate_window();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<f32> {
        Bounds::new(point(x, y), size(w, h))
    }

    /// A window at (100, 100): a control at (50, 40) sized 80×30 has its
    /// bottom at 170 and its right edge at 230.
    #[test]
    fn hints_sit_below_or_beside_and_never_under_the_pointer() {
        let frame = point(100.0, 100.0);
        let anchor = bounds(50.0, 40.0, 80.0, 30.0);
        assert_eq!(
            place(frame, anchor, (120.0, 24.0), HintSide::Below, None),
            point(100.0 + 50.0 + 40.0 - 60.0, 170.0 + GAP)
        );
        assert_eq!(
            place(frame, anchor, (120.0, 24.0), HintSide::Right, None),
            point(230.0 + SIDE_GAP, 155.0 - 12.0)
        );
    }

    #[test]
    fn a_hint_is_slid_back_inside_the_screen() {
        let frame = point(0.0, 0.0);
        let screen = Some(bounds(0.0, 0.0, 400.0, 300.0));
        // Centred below a control at the right edge: pulled left.
        let edge = bounds(360.0, 10.0, 40.0, 20.0);
        let at = place(frame, edge, (120.0, 24.0), HintSide::Below, screen);
        assert_eq!(at.x, 400.0 - 120.0 - SCREEN_MARGIN);
        // Below a control at the bottom: kept above the screen's bottom.
        let low = bounds(10.0, 290.0, 40.0, 10.0);
        let at = place(frame, low, (120.0, 24.0), HintSide::Below, screen);
        assert_eq!(at.y, 300.0 - 24.0 - SCREEN_MARGIN);
    }
}
