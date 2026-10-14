//! Hints and passing messages drawn by AppKit in a little window of their
//! own, for a host whose window has a native view over it: a tooltip
//! drawn inside the window would be hidden wherever the view is, or cut
//! off at its edge.
//!
//! The panel never activates and lets the pointer through, sits beside or
//! below what it names rather than under the pointer, and follows the
//! window as a child of it. [`crate::hinted`] uses [`attach`] when the host
//! has asked for native hints; a host reaches for [`toast`] itself, for a
//! message that has nowhere else to go.

use std::{cell::RefCell, time::Duration};

use gpui::{
    Bounds, Context, Div, InteractiveElement, MouseButton, ParentElement, Pixels, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas,
};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSPanel, NSTextField, NSTitlePosition,
    NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::lighting;
use crate::overlay::{Hint, HintSide};
use crate::palette::Palette;
use crate::state::ControlHost;

use super::ns_window_of;

/// How long the pointer rests on something before its hint shows.
const DELAY: Duration = Duration::from_millis(450);
/// How long a passing message stays.
const TOAST: Duration = Duration::from_millis(2800);
/// Between a hint and what it names.
const GAP: f64 = 6.0;
const SIDE_GAP: f64 = 8.0;
/// Kept clear of the screen's edges.
const SCREEN_MARGIN: f64 = 4.0;

/// The panel, its rounded frame and its text, made once.
type Panel = (Retained<NSPanel>, Retained<NSBox>, Retained<NSTextField>);

thread_local! {
    static HINT: RefCell<Option<Panel>> = const { RefCell::new(None) };
    /// A passing message's own window, so a hint doesn't take it away.
    static TOAST_PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

/// Gives `element` a hint in a panel of its own, shown after the pointer
/// has rested on it for a moment and taken away when the pointer leaves,
/// the element is pressed, or [`dismiss`] is called. The hint is keyed by
/// the element's id, so two elements never fight over one panel.
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
    // By window too: each window draws its own.
    let id = element
        .interactivity()
        .element_id
        .clone()
        .map_or_else(|| text.clone(), |id| format!("{id:?}"));
    let key = format!("{}:{side:?}:{id}", cx.entity_id());
    let weak = cx.entity().downgrade();
    let anchor_key = key.clone();
    element
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            hover(this, key.clone(), text.clone(), side, *hovered, window, cx);
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, _| dismiss(this)),
        )
        .child(
            // Records where the hinted element is drawn, for the panel to
            // sit beside.
            canvas(
                move |bounds, _, cx| {
                    if let Some(host) = weak.upgrade() {
                        host.update(cx, |host, _| {
                            host.control_state_mut().measure(anchor_key.clone(), bounds);
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
    key: String,
    text: String,
    side: HintSide,
    hovered: bool,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let state = view.control_state_mut();
    if !hovered {
        if state.hint_key.as_deref() == Some(key.as_str()) {
            state.hint_key = None;
            state.hint_timer = None;
            hide();
        }
        return;
    }
    state.hint_key = Some(key.clone());
    state.hint_timer = Some(cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(DELAY).await;
        let _ = this.update_in(cx, |host, window, _| {
            let state = host.control_state();
            if state.hint_key.as_deref() != Some(key.as_str()) {
                return;
            }
            let palette = state.palette();
            if let Some(bounds) = state.measured(key.clone()) {
                let (ns_window, _) = ns_window_of(window);
                show_in(&HINT, ns_window, &text, bounds, side, palette);
            }
        });
    }));
}

/// A click, a key or leaving the window: no hint. For whatever the host
/// knows takes the pointer away without a hover-out, such as a hinted
/// element being removed from under it.
pub fn dismiss<V: ControlHost>(view: &mut V) {
    let state = view.control_state_mut();
    state.hint_key = None;
    state.hint_timer = None;
    hide();
}

/// Hides the hint, if one is showing.
pub fn hide() {
    order_out(&HINT);
}

/// Shows `text` under `anchor` (window-relative, as GPUI lays out) in the
/// host's window for a few seconds: for what an action says when the
/// place such messages sit isn't showing.
pub fn toast<V: ControlHost>(
    view: &mut V,
    text: &str,
    anchor: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let palette = view.control_state().palette();
    let (ns_window, _) = ns_window_of(window);
    show_in(
        &TOAST_PANEL,
        ns_window,
        text,
        anchor,
        HintSide::Below,
        palette,
    );
    view.control_state_mut().toast_timer = Some(cx.spawn(async move |_, cx| {
        cx.background_executor().timer(TOAST).await;
        hide_toast();
    }));
}

/// Takes the passing message away.
pub fn hide_toast() {
    order_out(&TOAST_PANEL);
}

fn order_out(slot: &'static std::thread::LocalKey<RefCell<Option<Panel>>>) {
    slot.with(|panel| {
        if let Some((panel, _, _)) = panel.borrow().as_ref()
            && panel.isVisible()
        {
            panel.orderOut(None);
        }
    });
}

fn ns_color(color: gpui::Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from(color.red),
        f64::from(color.green),
        f64::from(color.blue),
        f64::from(color.alpha),
    )
}

/// The panel's frame for `anchor` in a window whose AppKit frame is
/// `frame`: centred below it, or beside it to the right, then kept inside
/// `screen`. Window coordinates come top-left as GPUI lays out; AppKit's
/// go bottom-left, so the window's top is where `y` is measured from.
fn place(
    frame: Bounds<f64>,
    anchor: Bounds<f64>,
    (width, height): (f64, f64),
    side: HintSide,
    screen: Option<Bounds<f64>>,
) -> (f64, f64) {
    let top = frame.origin.y + frame.size.height - anchor.origin.y;
    let (mut x, mut y) = match side {
        HintSide::Below => (
            frame.origin.x + anchor.origin.x + anchor.size.width / 2.0 - width / 2.0,
            top - anchor.size.height - GAP - height,
        ),
        HintSide::Right => (
            frame.origin.x + anchor.origin.x + anchor.size.width + SIDE_GAP,
            top - anchor.size.height / 2.0 - height / 2.0,
        ),
    };
    if let Some(screen) = screen {
        x = x.clamp(
            screen.origin.x + SCREEN_MARGIN,
            screen.origin.x + screen.size.width - width - SCREEN_MARGIN,
        );
        y = y.max(screen.origin.y + SCREEN_MARGIN);
    }
    (x, y)
}

fn show_in(
    slot: &'static std::thread::LocalKey<RefCell<Option<Panel>>>,
    ns_window: usize,
    text: &str,
    anchor: Bounds<Pixels>,
    side: HintSide,
    palette: Palette,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if ns_window == 0 {
        return;
    }
    // SAFETY: the host's own window, alive while it is.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    slot.with(|panel| {
        let mut panel = panel.borrow_mut();
        let (hint, frame_box, label) = panel.get_or_insert_with(|| {
            let hint = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
                NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::Buffered,
                false,
            );
            hint.setOpaque(false);
            hint.setBackgroundColor(Some(&NSColor::clearColor()));
            hint.setHasShadow(true);
            hint.setIgnoresMouseEvents(true);
            // SAFETY: plain property setter on a panel we own.
            unsafe { hint.setReleasedWhenClosed(false) };
            let frame_box = NSBox::new(mtm);
            frame_box.setBoxType(NSBoxType::Custom);
            frame_box.setTitlePosition(NSTitlePosition::NoTitle);
            frame_box.setCornerRadius(6.0);
            frame_box.setBorderWidth(0.5);
            frame_box.setContentViewMargins(NSSize::new(0.0, 0.0));
            let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            frame_box.addSubview(&label);
            hint.setContentView(Some(&frame_box));
            (hint, frame_box, label)
        });
        // The same surface a tooltip drawn in the window would have.
        let dark = palette.is_dark;
        let fill = if dark {
            palette.soft_fill
        } else {
            palette.field_surface
        };
        frame_box.setFillColor(&ns_color(fill));
        frame_box.setBorderColor(&ns_color(lighting::rim(fill, dark)));
        label.setTextColor(Some(&ns_color(palette.text_primary)));
        label.setStringValue(&NSString::from_str(text));
        label.sizeToFit();
        let size = label.frame().size;
        let (pad_x, pad_y) = (8.0, 4.0);
        let (width, height) = (size.width + pad_x * 2.0, size.height + pad_y * 2.0);
        label.setFrameOrigin(NSPoint::new(pad_x, pad_y));
        // GPUI lays out in the content view, below any title bar.
        let frame = window.contentRectForFrameRect(window.frame());
        let frame = Bounds::new(
            gpui::point(frame.origin.x, frame.origin.y),
            gpui::size(frame.size.width, frame.size.height),
        );
        let anchor = Bounds::new(
            gpui::point(
                f64::from(f32::from(anchor.origin.x)),
                f64::from(f32::from(anchor.origin.y)),
            ),
            gpui::size(
                f64::from(f32::from(anchor.size.width)),
                f64::from(f32::from(anchor.size.height)),
            ),
        );
        let screen = window.screen().map(|screen| {
            let visible = screen.visibleFrame();
            Bounds::new(
                gpui::point(visible.origin.x, visible.origin.y),
                gpui::size(visible.size.width, visible.size.height),
            )
        });
        let (x, y) = place(frame, anchor, (width, height), side, screen);
        hint.setFrame_display(
            NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)),
            true,
        );
        if hint
            .parentWindow()
            .is_none_or(|parent| !std::ptr::eq(&*parent, window))
        {
            if let Some(parent) = hint.parentWindow() {
                parent.removeChildWindow(hint);
            }
            // SAFETY: both windows are live; the panel floats above its parent.
            unsafe { window.addChildWindow_ordered(hint, NSWindowOrderingMode::Above) };
        }
        hint.orderFront(None);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f64, y: f64, w: f64, h: f64) -> Bounds<f64> {
        Bounds::new(gpui::point(x, y), gpui::size(w, h))
    }

    /// A window 600 tall whose AppKit origin is at (100, 100): a control at
    /// GPUI (50, 40) sized 80×30 has its top at AppKit y 660 and its bottom
    /// at 630.
    #[test]
    fn hints_sit_below_or_beside_and_never_under_the_pointer() {
        let frame = bounds(100.0, 100.0, 800.0, 600.0);
        let anchor = bounds(50.0, 40.0, 80.0, 30.0);
        let (x, y) = place(frame, anchor, (120.0, 24.0), HintSide::Below, None);
        assert_eq!((x, y), (100.0 + 50.0 + 40.0 - 60.0, 630.0 - GAP - 24.0));
        let (x, y) = place(frame, anchor, (120.0, 24.0), HintSide::Right, None);
        assert_eq!((x, y), (100.0 + 130.0 + SIDE_GAP, 645.0 - 12.0));
    }

    #[test]
    fn a_hint_is_slid_back_inside_the_screen() {
        let frame = bounds(0.0, 0.0, 400.0, 300.0);
        let screen = Some(bounds(0.0, 0.0, 400.0, 300.0));
        // Centred below a control at the right edge: pulled left.
        let edge = bounds(360.0, 10.0, 40.0, 20.0);
        let (x, _) = place(frame, edge, (120.0, 24.0), HintSide::Below, screen);
        assert_eq!(x, 400.0 - 120.0 - SCREEN_MARGIN);
        // Below a control at the bottom: kept above the screen's bottom.
        let low = bounds(10.0, 290.0, 40.0, 10.0);
        let (_, y) = place(frame, low, (120.0, 24.0), HintSide::Below, screen);
        assert_eq!(y, SCREEN_MARGIN);
    }
}
