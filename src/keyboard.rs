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
//! `on_key_down`, and need no setup at all.
//!
//! Three keys are the same in every application but cannot be any one
//! control's: Tab and Shift-Tab, which walk the window, and Escape, which
//! closes whatever is open. Text editing is the same everywhere too. Those
//! the toolkit binds itself, to its own actions, in [`bind_keys`], and
//! [`handle_keys`] puts the handlers for them on the host's root:
//!
//! ```ignore
//! // Once, when the application starts:
//! vampir::bind_keys(cx);
//! // On the root element of every view that hosts controls:
//! vampir::root(div().id("root"), self, cx)
//!     .child(..)
//! ```
//!
//! Everything else — ⌘K, ⌘S, the application's own keys — stays the
//! host's.

use gpui::{
    App, Context, Div, ElementId, FocusHandle, InteractiveElement, KeyBinding, KeyDownEvent, Menu,
    MenuItem as OsMenuItem, MouseDownEvent, OsAction, Stateful, Styled, Window, actions, div, px,
};

use crate::lighting;
use crate::palette::Palette;
use crate::state::ControlHost;
use crate::text_input as text;

// The toolkit's own actions, which `bind_keys` binds and `handle_keys`
// handles: Tab, Shift-Tab and Escape. Escape reaches whatever holds the
// keyboard first — a pop-up list, a menu, a dialog — and the root only if
// none of them took it.
actions!(vampir, [FocusNext, FocusPrevious, Dismiss]);

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

/// The bindings that are the same in every application: Tab and Shift-Tab
/// walking the window, Escape closing whatever is open, and the text
/// editing keys in the `TextInput` context. `secondary` is ⌘ on macOS and
/// Ctrl everywhere else, so one binding serves both; the rest of the text
/// keymap is the one place the platforms genuinely disagree — ⌥ moves by
/// word on a Mac, Ctrl does elsewhere — so it is written out twice.
///
/// [`bind_keys`] installs them. This is the list on its own, for a host
/// that wants to look at it or leave some out.
pub fn standard_bindings() -> Vec<KeyBinding> {
    const TEXT: Option<&str> = Some("TextInput");
    let mut bindings = vec![
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("escape", Dismiss, None),
        KeyBinding::new("backspace", text::Backspace, TEXT),
        KeyBinding::new("delete", text::Delete, TEXT),
        KeyBinding::new("left", text::Left, TEXT),
        KeyBinding::new("right", text::Right, TEXT),
        KeyBinding::new("up", text::Up, TEXT),
        KeyBinding::new("down", text::Down, TEXT),
        KeyBinding::new("shift-left", text::SelectLeft, TEXT),
        KeyBinding::new("shift-right", text::SelectRight, TEXT),
        KeyBinding::new("shift-up", text::SelectUp, TEXT),
        KeyBinding::new("shift-down", text::SelectDown, TEXT),
        KeyBinding::new("home", text::Home, TEXT),
        KeyBinding::new("end", text::End, TEXT),
        KeyBinding::new("shift-home", text::SelectToHome, TEXT),
        KeyBinding::new("shift-end", text::SelectToEnd, TEXT),
        KeyBinding::new("enter", text::Enter, TEXT),
        KeyBinding::new("secondary-a", text::SelectAll, TEXT),
        KeyBinding::new("secondary-c", text::Copy, TEXT),
        KeyBinding::new("secondary-v", text::Paste, TEXT),
        KeyBinding::new("secondary-x", text::Cut, TEXT),
        KeyBinding::new("secondary-z", text::Undo, TEXT),
        KeyBinding::new("secondary-shift-z", text::Redo, TEXT),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("alt-backspace", text::DeleteWordLeft, TEXT),
            KeyBinding::new("cmd-backspace", text::DeleteToLineStart, TEXT),
            KeyBinding::new("alt-left", text::WordLeft, TEXT),
            KeyBinding::new("alt-right", text::WordRight, TEXT),
            KeyBinding::new("alt-shift-left", text::SelectWordLeft, TEXT),
            KeyBinding::new("alt-shift-right", text::SelectWordRight, TEXT),
            KeyBinding::new("cmd-left", text::Home, TEXT),
            KeyBinding::new("cmd-right", text::End, TEXT),
            KeyBinding::new("cmd-shift-left", text::SelectToHome, TEXT),
            KeyBinding::new("cmd-shift-right", text::SelectToEnd, TEXT),
            KeyBinding::new("cmd-up", text::DocumentStart, TEXT),
            KeyBinding::new("cmd-down", text::DocumentEnd, TEXT),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-backspace", text::DeleteWordLeft, TEXT),
            KeyBinding::new("ctrl-left", text::WordLeft, TEXT),
            KeyBinding::new("ctrl-right", text::WordRight, TEXT),
            KeyBinding::new("ctrl-shift-left", text::SelectWordLeft, TEXT),
            KeyBinding::new("ctrl-shift-right", text::SelectWordRight, TEXT),
            KeyBinding::new("ctrl-home", text::DocumentStart, TEXT),
            KeyBinding::new("ctrl-end", text::DocumentEnd, TEXT),
        ]);
    }
    bindings
}

/// Binds the keys that are the same in every application — see
/// [`standard_bindings`]. Call it once, when the application starts. A
/// host's own bindings go in after it; GPUI gives a later binding for the
/// same key precedence, so a host that needs Escape for something of its
/// own can still take it.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(standard_bindings());
}

/// Puts the handlers for the standard keys on the host's root element, so
/// the actions [`bind_keys`] binds have somewhere to arrive when the focused
/// control has not taken them itself. Tab and Shift-Tab go through
/// [`move_focus`]; Escape closes any pop-up list, menu or command palette
/// still open; and a mouse press anywhere marks the mouse as being in
/// charge, so that the next Tab shows where the keyboard is rather than
/// moving it.
///
/// It also gives the root the toolkit's root focus handle
/// ([`ControlState::root_focus`](crate::ControlState::root_focus)), which is
/// where the keyboard goes when whatever had it has gone: an overlay
/// closing with nothing to hand back to, a host switching pages
/// ([`ControlState::focus_root`](crate::ControlState::focus_root)). GPUI
/// dispatches nothing from a handle that is no longer in the tree — not even
/// the root's own shortcuts — so the keyboard always needs somewhere live.
///
/// The host adds its own `on_action`s after this. An Escape nothing of the
/// toolkit's was open for is passed on to them, so a host closes its own
/// overlays with the same key by listening for [`Dismiss`] too.
pub fn handle_keys<E: InteractiveElement, V: ControlHost>(
    root: E,
    view: &V,
    cx: &mut Context<V>,
) -> E {
    root.track_focus(&view.control_state().root_focus(cx))
        .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
            move_focus(this, window, cx, false);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
            move_focus(this, window, cx, true);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
            if !this.control_state_mut().dismiss_overlays(window, cx) {
                cx.propagate();
                return;
            }
            cx.notify();
        }))
        .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _window, _cx| {
            this.control_state_mut().ring_hidden = true;
        }))
}

/// Everything the toolkit needs on a host's root element: [`handle_keys`]
/// and [`crate::handle_mouse`] together.
///
/// ```ignore
/// vampir::root(div().id("root"), self, cx)
///     .on_action(cx.listener(Self::toggle_palette))  // the host's own
///     .child(..)
/// ```
pub fn root<E: InteractiveElement, V: ControlHost>(root: E, view: &V, cx: &mut Context<V>) -> E {
    handle_keys(crate::state::handle_mouse(root, cx), view, cx)
}

/// The Edit menu for the menu bar: Undo, Redo, Cut, Copy, Paste and Select
/// All, wired to the text input's actions.
///
/// Each item carries an `OsAction`, so macOS routes it through the responder
/// chain to whatever text is focused — which is what makes one Edit menu
/// work for the toolkit's [`TextInput`](crate::TextInput) and for the
/// system's own fields alike. Put it in `cx.set_menus(..)` between the
/// application's own menus; see the macOS guide for the rest of the bar.
pub fn edit_menu() -> Menu {
    Menu::new("Edit").items([
        OsMenuItem::os_action("Undo", text::Undo, OsAction::Undo),
        OsMenuItem::os_action("Redo", text::Redo, OsAction::Redo),
        OsMenuItem::separator(),
        OsMenuItem::os_action("Cut", text::Cut, OsAction::Cut),
        OsMenuItem::os_action("Copy", text::Copy, OsAction::Copy),
        OsMenuItem::os_action("Paste", text::Paste, OsAction::Paste),
        OsMenuItem::os_action("Select All", text::SelectAll, OsAction::SelectAll),
    ])
}

/// Moves the keyboard to the next control, or back to the previous one.
///
/// What Tab and Shift-Tab do, by way of [`handle_keys`]. It closes any
/// pop-up the keyboard is walking away from, keeps Tab cycling inside a
/// modal dialog while one is up, and otherwise moves to the next stop in
/// the window.
///
/// One more thing, for a hand that has just come from the mouse: the first
/// Tab after a mouse press only shows where the keyboard already is. The
/// ring is hidden while the mouse is in charge, so a Tab that moved would
/// look as though it had skipped a control — you clicked one thing and the
/// ring appeared on the next.
///
/// None of it can be done from inside the dialog: Tab is bound, and in GPUI
/// a keystroke that matches a binding is dispatched as that action and
/// never reaches a key listener — so the only place the dialog can be
/// consulted is here, on the action's side of that binding.
pub fn move_focus<V: ControlHost>(view: &mut V, window: &mut Window, cx: &mut App, backward: bool) {
    let state = view.control_state_mut();
    // A dropdown is a question; walking to another control answers it by
    // leaving, and a list still on screen looks as though the next key
    // pressed will land in it.
    state.dismiss_popups();
    let reveal = std::mem::take(&mut state.ring_hidden) && window.focused(cx).is_some();
    if reveal {
        return;
    }
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
    /// Escape, when no binding has claimed it. Once [`bind_keys`] has run it
    /// arrives as the [`Dismiss`] action instead, and the pop-ups listen for
    /// both.
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
    use super::{Key, KeyBinding, Orientation, nudge, nudge_stepped, standard_bindings, step};

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

    /// The Edit menu has the six clipboard and history items every
    /// application's does, in the order every application lists them.
    #[test]
    fn the_edit_menu_is_the_usual_one() {
        let menu = super::edit_menu();
        assert_eq!(menu.name.as_ref(), "Edit");
        let names: Vec<String> = menu
            .items
            .iter()
            .filter_map(|item| match item {
                gpui::MenuItem::Action {
                    name, os_action, ..
                } => {
                    assert!(os_action.is_some(), "{name} needs an OsAction");
                    Some(name.to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["Undo", "Redo", "Cut", "Copy", "Paste", "Select All"]
        );
    }

    #[test]
    fn the_standard_bindings_parse_and_do_not_collide() {
        let bindings = standard_bindings();
        let keys = |binding: &KeyBinding| -> String {
            binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.unparse())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut seen = std::collections::HashSet::new();
        for binding in &bindings {
            let scoped = binding.predicate().is_some();
            assert!(
                seen.insert((keys(binding), scoped)),
                "{} is bound twice",
                keys(binding)
            );
        }
        // The window-wide keys are the unscoped ones; everything else is a
        // text field's, and stays out of the way of every other control.
        let unscoped: Vec<String> = bindings
            .iter()
            .filter(|binding| binding.predicate().is_none())
            .map(keys)
            .collect();
        assert_eq!(unscoped, ["tab", "shift-tab", "escape"]);
    }
}
