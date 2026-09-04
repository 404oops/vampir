//! What a key press means to a control that has focus.
//!
//! There are two kinds of shortcut in an application and they belong to
//! different owners. ⌘K opens *this app's* command palette; which key does
//! that is a decision only the host can make, so the toolkit ships actions
//! and leaves the binding alone. Space on a focused button is not that kind
//! of thing. It is what a button *is*, on every platform, and a toolkit that
//! made every host bind it would ship inaccessible by default and stay that
//! way in most applications built on it.
//!
//! So the keys in [`Key`] are handled by the controls themselves, through
//! `on_key_down`, and need no setup at all. Everything else stays the
//! host's.
//!
//! The one thing the host still has to bind is Tab, because moving focus is
//! a window-wide decision rather than any one control's:
//!
//! ```ignore
//! KeyBinding::new("tab", FocusNext, None),
//! KeyBinding::new("shift-tab", FocusPrevious, None),
//! // ...and in the handlers:
//! window.focus_next(cx);
//! window.focus_prev(cx);
//! ```

use gpui::{
    App, Div, ElementId, FocusHandle, InteractiveElement, KeyDownEvent, Stateful, Styled, Window,
    div, px,
};

use crate::lighting;
use crate::palette::Palette;
use crate::state::ControlHost;

/// Makes a control a tab stop and rings it when the keyboard lands on it.
///
/// `own_shadows` are the control's own lighting. The ring is added to them
/// rather than replacing them, so a focused button still reads as raised.
///
/// The element must already have an id: GPUI hangs the focus handle off the
/// element's state, and an element with no id has none to hang it on, so it
/// would take focus one frame and lose it the next.
///
/// A disabled control must not be passed through here. Tab stopping on
/// something nobody can operate is worse than not reaching it at all: it
/// reads as the keyboard being stuck.
/// The focus ring, laid over a control as its **last** child.
///
/// A box shadow paints *behind* the element that owns it. On a filled
/// control that reads as a ring; on a transparent one it shows straight
/// through as a block of colour; and under a neighbour, or inside anything
/// that clips — a scrolling tab bar, a tree with hidden overflow — it is
/// cut off or lost entirely.
///
/// So the ring is an element rather than a style. As the last child it
/// paints over the control's own content, and it lies exactly on the
/// control's bounds, so there is nothing outside for a clip to take. The
/// same recipe then works for a filled control and a bare one.
///
/// It is also the tab stop: its hitbox covers the control, so clicking
/// anywhere on the control focuses it and the keyboard picks up where the
/// mouse left off. The control it goes on needs `.relative()`.
///
/// ```ignore
/// div().id("save").relative()
///     .child(label)
///     .child(keyboard::ring("save-ring", CONTROL_RADIUS, palette).on_key_down(activate))
/// ```
pub fn ring(id: impl Into<ElementId>, radius: f32, palette: Palette) -> Stateful<Div> {
    dress(div().id(id.into()).tab_index(0), radius, palette)
}

/// The same ring for the current option of a composite control.
///
/// It carries the group's shared handle rather than one of its own, so the
/// ring rides whichever option is current: the group stays one stop in the
/// tab order, and the ring marks the thing the arrows are pointing at.
pub fn ring_for(focus: &FocusHandle, radius: f32, palette: Palette) -> Div {
    dress(div().track_focus(focus), radius, palette)
}

fn dress<E: InteractiveElement + Styled>(el: E, radius: f32, palette: Palette) -> E {
    let (accent, dark) = (palette.accent, palette.is_dark);
    el.absolute()
        .inset_0()
        .rounded(px(radius))
        // Present but invisible until focused, so the ring appearing costs
        // no layout: a border on an absolutely-placed box draws inward, and
        // moves nothing.
        .border_2()
        .border_color(gpui::transparent_black())
        // `focus_visible` rather than `focus`: the ring is a keyboard
        // affordance, and drawing it on a control someone has just clicked
        // tells them something they already know.
        .focus_visible(move |style| {
            style.border_color(accent).shadow(vec![lighting::glow(
                accent,
                if dark { 0.8 } else { 0.5 },
                4.0,
            )])
        })
}

/// Moves the keyboard to the next control, or back to the previous one.
///
/// Moving focus is the one keyboard job the toolkit leaves to the host, and
/// this is what the host's Tab handler should call. It closes any pop-up the
/// keyboard is walking away from, keeps Tab cycling inside a modal dialog
/// while one is up, and otherwise moves to the next stop in the window.
///
/// It cannot be done from inside the dialog: Tab is bound by the host, and
/// in GPUI a keystroke that matches a binding is dispatched as that action
/// and never reaches a key listener — so the only place the dialog can be
/// consulted is here, on the host's side of that binding.
pub fn move_focus<V: ControlHost>(view: &mut V, window: &mut Window, cx: &mut App, backward: bool) {
    view.control_state_mut().dismiss_popups();
    if let Some(next) = view.control_state().trap_next(window, backward) {
        window.focus(&next, cx);
        return;
    }
    if backward {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
}

/// A key press a control acts on itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// Space or Enter: do whatever the control does.
    Activate,
    Left,
    Right,
    Up,
    Down,
    /// The first item, or the minimum of a range.
    Home,
    /// The last item, or the maximum of a range.
    End,
    /// A larger step through a range.
    PageUp,
    /// A larger step through a range.
    PageDown,
    /// Escape. Only pop-ups act on it; everything else leaves it to the host.
    Dismiss,
}

/// Reads a key press, or `None` if the control should leave it alone.
///
/// A press carrying ⌘, ⌃, ⌥ or fn is never one of these. ⌘← is "go to the
/// start of the line" and ⌥→ is "one word right"; a slider that treated
/// them as its own would swallow bindings the host had every right to make.
/// Shift is allowed through, because shift-arrow is a range gesture that
/// controls without a selection can simply ignore.
pub fn key(event: &KeyDownEvent) -> Option<Key> {
    let modifiers = &event.keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
        return None;
    }
    Some(match event.keystroke.key.as_str() {
        "space" | "enter" => Key::Activate,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "escape" => Key::Dismiss,
        _ => return None,
    })
}

/// Which arrows move through a group.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    /// Left and right, for a segmented control or a row of tabs.
    Horizontal,
    /// Up and down, for a radio group or a menu.
    Vertical,
    /// Both, for a wrapping field of chips or a grid of swatches, where a
    /// reader cannot tell which axis the layout used.
    Either,
}

/// Where `key` moves within a group of `len` items, or `None` if it does
/// not move at all.
///
/// Movement wraps. A group of choices has no end to fall off: pressing down
/// on the last radio button lands on the first, which is what every platform
/// does and what stops a keyboard user getting stuck at the bottom of a list
/// wondering whether the control is broken.
pub fn step(key: Key, orientation: Orientation, current: usize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    // A stale index — the host shrank the list between frames — is pulled
    // back to the end rather than wrapping from somewhere that no longer
    // exists.
    let current = current.min(last);

    let backward = match orientation {
        Orientation::Horizontal => key == Key::Left,
        Orientation::Vertical => key == Key::Up,
        Orientation::Either => matches!(key, Key::Left | Key::Up),
    };
    let forward = match orientation {
        Orientation::Horizontal => key == Key::Right,
        Orientation::Vertical => key == Key::Down,
        Orientation::Either => matches!(key, Key::Right | Key::Down),
    };

    if backward {
        Some(if current == 0 { last } else { current - 1 })
    } else if forward {
        Some(if current == last { 0 } else { current + 1 })
    } else {
        match key {
            Key::Home => Some(0),
            Key::End => Some(last),
            _ => None,
        }
    }
}

/// Where `key` moves along a continuous range of `0.0..=1.0`.
///
/// `step` is one arrow press. Page is ten of them, which is the ratio every
/// platform slider uses, and Home and End go to the ends. Returns `None`
/// when the key does not move the value, so a caller can leave it untouched
/// rather than writing back an identical one and repainting for nothing.
pub fn nudge(key: Key, orientation: Orientation, value: f32, step_by: f32) -> Option<f32> {
    let page = step_by * 10.0;
    let delta = match (key, orientation) {
        (Key::Left, Orientation::Horizontal | Orientation::Either) => -step_by,
        (Key::Right, Orientation::Horizontal | Orientation::Either) => step_by,
        // Down decreases, whichever way the track runs: a value going down
        // is the shared meaning, and it is what a vertical track paints
        // lower anyway.
        (Key::Down, Orientation::Vertical | Orientation::Either) => -step_by,
        (Key::Up, Orientation::Vertical | Orientation::Either) => step_by,
        (Key::PageDown, _) => -page,
        (Key::PageUp, _) => page,
        (Key::Home, _) => return Some(0.0),
        (Key::End, _) => return Some(1.0),
        _ => return None,
    };
    let moved = (value + delta).clamp(0.0, 1.0);
    (moved != value).then_some(moved)
}

/// The same as [`nudge`], for a range divided into `stops` notches.
///
/// A stepped slider moves one notch per press rather than a fraction of the
/// track, because the notches are the values — landing between two of them
/// and snapping back would read as the key having done nothing.
pub fn nudge_stepped(key: Key, orientation: Orientation, value: f32, stops: u32) -> Option<f32> {
    if stops == 0 {
        return nudge(key, orientation, value, 0.05);
    }
    let current = (value * stops as f32).round() as i64;
    let moved = match (key, orientation) {
        (Key::Left, Orientation::Horizontal | Orientation::Either) => current - 1,
        (Key::Right, Orientation::Horizontal | Orientation::Either) => current + 1,
        (Key::Down, Orientation::Vertical | Orientation::Either) => current - 1,
        (Key::Up, Orientation::Vertical | Orientation::Either) => current + 1,
        (Key::PageDown, _) => current - (stops as i64 / 4).max(1),
        (Key::PageUp, _) => current + (stops as i64 / 4).max(1),
        (Key::Home, _) => 0,
        (Key::End, _) => stops as i64,
        _ => return None,
    };
    let moved = moved.clamp(0, stops as i64) as f32 / stops as f32;
    (moved != value).then_some(moved)
}

#[cfg(test)]
mod tests {
    use super::{Key, Orientation, nudge, nudge_stepped, step};

    #[test]
    fn a_group_wraps_at_both_ends() {
        assert_eq!(step(Key::Down, Orientation::Vertical, 2, 3), Some(0));
        assert_eq!(step(Key::Up, Orientation::Vertical, 0, 3), Some(2));
    }

    #[test]
    fn home_and_end_reach_the_ends_from_anywhere() {
        assert_eq!(step(Key::Home, Orientation::Vertical, 2, 4), Some(0));
        assert_eq!(step(Key::End, Orientation::Horizontal, 0, 4), Some(3));
    }

    /// A horizontal group ignores up and down, so the arrows that move
    /// *between* controls are not swallowed by one of them.
    #[test]
    fn a_group_only_answers_to_its_own_axis() {
        assert_eq!(step(Key::Up, Orientation::Horizontal, 1, 3), None);
        assert_eq!(step(Key::Left, Orientation::Vertical, 1, 3), None);
        assert_eq!(step(Key::Left, Orientation::Either, 1, 3), Some(0));
        assert_eq!(step(Key::Up, Orientation::Either, 1, 3), Some(0));
    }

    #[test]
    fn an_empty_group_does_not_move() {
        assert_eq!(step(Key::Down, Orientation::Vertical, 0, 0), None);
    }

    /// A stale index — the host shrank the list between frames — lands on
    /// the last item rather than panicking or wrapping from nowhere.
    #[test]
    fn an_index_past_the_end_is_pulled_back() {
        assert_eq!(step(Key::Up, Orientation::Vertical, 9, 3), Some(1));
    }

    #[test]
    fn a_range_stops_at_its_ends_rather_than_wrapping() {
        assert_eq!(
            nudge(Key::Right, Orientation::Horizontal, 0.98, 0.05),
            Some(1.0)
        );
        // Already there: no move, so no repaint.
        assert_eq!(nudge(Key::Right, Orientation::Horizontal, 1.0, 0.05), None);
        assert_eq!(nudge(Key::Left, Orientation::Horizontal, 0.0, 0.05), None);
    }

    #[test]
    fn a_page_is_ten_arrow_presses() {
        assert_eq!(
            nudge(Key::PageUp, Orientation::Horizontal, 0.0, 0.05),
            Some(0.5)
        );
    }

    #[test]
    fn down_lowers_a_value_and_up_raises_it() {
        assert_eq!(nudge(Key::Down, Orientation::Vertical, 0.5, 0.1), Some(0.4));
        assert_eq!(nudge(Key::Up, Orientation::Vertical, 0.5, 0.1), Some(0.6));
    }

    #[test]
    fn a_stepped_range_moves_one_notch_at_a_time() {
        // Four stops: 0, 0.25, 0.5, 0.75, 1.0.
        assert_eq!(
            nudge_stepped(Key::Right, Orientation::Horizontal, 0.5, 4),
            Some(0.75)
        );
        assert_eq!(
            nudge_stepped(Key::Left, Orientation::Horizontal, 0.5, 4),
            Some(0.25)
        );
        assert_eq!(
            nudge_stepped(Key::End, Orientation::Horizontal, 0.5, 4),
            Some(1.0)
        );
        assert_eq!(
            nudge_stepped(Key::Right, Orientation::Horizontal, 1.0, 4),
            None
        );
    }

    /// A value sitting between notches snaps to the nearest one and then
    /// moves, rather than moving and then snapping back to where it was.
    #[test]
    fn a_stepped_range_lands_on_a_notch_from_between_two() {
        assert_eq!(
            nudge_stepped(Key::Right, Orientation::Horizontal, 0.6, 4),
            Some(0.75)
        );
    }
}
