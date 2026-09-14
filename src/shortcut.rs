//! Shortcut recorder: a field that captures the next key chord.
//!
//! It records what was pressed and hands back a description. Binding the
//! result is the host's business, because only the host knows what its
//! actions are and gpui wants a keymap, not a widget, to own them.

use std::rc::Rc;

use gpui::{
    Context, ElementId, KeyDownEvent, KeyUpEvent, Keystroke, SharedString, Window, div, prelude::*,
    px,
};

use crate::controls::WidgetContext;
use crate::controls::{CONTROL_HEIGHT, CONTROL_RADIUS};
use crate::lighting;
use crate::state::{ComboId, ControlHost};

/// A captured key chord, in gpui's own keystroke notation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    /// What to give a `KeyBinding`, for instance `cmd-shift-p`.
    pub keystroke: SharedString,
    /// The same chord for a person to read, for instance `⌘⇧P`.
    pub display: SharedString,
}

/// Turns a key event into a chord, or `None` if it is not one worth
/// recording.
///
/// A bare modifier is skipped: while someone holds Command on the way to
/// Command-S, every one of those presses arrives here, and recording the
/// first would end the capture before they got to the letter.
pub fn chord_from(event: &KeyDownEvent) -> Option<Chord> {
    chord_for_keystroke(&event.keystroke)
}

fn chord_for_keystroke(keystroke: &Keystroke) -> Option<Chord> {
    let key = keystroke.key.as_str();
    if key.is_empty() || matches!(key, "cmd" | "ctrl" | "alt" | "shift" | "fn" | "function") {
        return None;
    }
    let modifiers = &keystroke.modifiers;

    let mut parts: Vec<&str> = Vec::new();
    if modifiers.control {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    if modifiers.platform {
        parts.push("cmd");
    }
    if modifiers.function {
        parts.push("fn");
    }
    parts.push(key);

    Some(Chord {
        keystroke: parts.join("-").into(),
        display: display_keystroke(keystroke).into(),
    })
}

/// A keystroke as the platform writes it: `⌘⇧S` on macOS, `Ctrl+Shift+S`
/// everywhere else.
///
/// Menus, tooltips and the recorder all go through here, so a shortcut is
/// never shown in one platform's notation on another — a `⌘` on a Windows
/// machine is a symbol with no key behind it.
pub fn display_keystroke(keystroke: &Keystroke) -> String {
    let modifiers = &keystroke.modifiers;
    let key = pretty_key(&keystroke.key);
    if cfg!(target_os = "macos") {
        // Apple's order: control, option, shift, command.
        let mut display = String::new();
        if modifiers.control {
            display.push('\u{2303}');
        }
        if modifiers.alt {
            display.push('\u{2325}');
        }
        if modifiers.shift {
            display.push('\u{21e7}');
        }
        if modifiers.platform {
            display.push('\u{2318}');
        }
        if modifiers.function {
            display.push_str("Fn");
        }
        display.push_str(&key);
        display
    } else {
        let mut parts: Vec<String> = Vec::new();
        if modifiers.control {
            parts.push("Ctrl".into());
        }
        if modifiers.platform {
            parts.push(
                if cfg!(target_os = "windows") {
                    "Win"
                } else {
                    "Super"
                }
                .into(),
            );
        }
        if modifiers.alt {
            parts.push("Alt".into());
        }
        if modifiers.shift {
            parts.push("Shift".into());
        }
        if modifiers.function {
            parts.push("Fn".into());
        }
        parts.push(key);
        parts.join("+")
    }
}

/// [`display_keystroke`] for a binding string such as `"secondary-shift-s"`,
/// which is how a host already writes its bindings. `secondary` is ⌘ on
/// macOS and Ctrl elsewhere, so one string labels both correctly. A string
/// that does not parse is shown as it is, which beats an empty label.
pub fn display(binding: &str) -> SharedString {
    Keystroke::parse(binding)
        .map(|keystroke| display_keystroke(&keystroke))
        .unwrap_or_else(|_| binding.to_string())
        .into()
}

/// A key's name as it appears on a key cap.
fn pretty_key(key: &str) -> String {
    match key {
        "enter" => "\u{21a9}".into(),
        "tab" => "\u{21e5}".into(),
        "space" => "Space".into(),
        "backspace" => "\u{232b}".into(),
        "delete" => "\u{2326}".into(),
        "escape" => "Esc".into(),
        "up" => "\u{2191}".into(),
        "down" => "\u{2193}".into(),
        "left" => "\u{2190}".into(),
        "right" => "\u{2192}".into(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// A field that shows a shortcut and, when clicked, captures the next chord.
///
/// Capturing keys means taking focus, and focus has to outlive a frame, so
/// the recorder's handle lives in [`ControlState`](crate::ControlState)
/// under `id` like a composite control's. While recording, Escape cancels
/// and every other chord is recorded and ends the capture.
/// Space or Enter starts recording when the field has keyboard focus.
pub fn shortcut_recorder<V: ControlHost>(
    id: ComboId,
    current: Option<&Chord>,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_record: impl Fn(&mut V, Chord, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let focus = view.control_state().focus(id, cx);
    let recording = view.control_state().recording == Some(id);
    let label: SharedString = match (recording, current) {
        (true, _) => "Press a shortcut\u{2026}".into(),
        (false, Some(chord)) => chord.display.clone(),
        (false, None) => "Not set".into(),
    };
    let on_record: Record<V> = Rc::new(on_record);
    let on_click_record = on_record.clone();

    div()
        .id(ElementId::Name(format!("{id}-recorder").into()))
        // The recorder is a tab stop like any other field: it is where a
        // chord gets typed, so the keyboard has to be able to get to it.
        .relative()
        .key_context("ShortcutRecorder")
        .h(px(CONTROL_HEIGHT))
        .w_full()
        .px(px(9.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CONTROL_RADIUS))
        .bg(palette.field_surface)
        .border_1()
        .border_color(if recording {
            palette.accent
        } else {
            palette.field_border
        })
        .shadow({
            let mut shadows = lighting::recessed(palette.is_dark);
            if recording {
                shadows.push(lighting::glow(palette.accent, 0.4, 4.0));
            }
            shadows
        })
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(if current.is_some() || recording {
            palette.text_primary
        } else {
            palette.text_secondary
        })
        .on_click(cx.listener(move |this, _event, window, cx| {
            start_recording(this, id, None, on_click_record.clone(), window, cx);
        }))
        .on_mouse_down_out(cx.listener(move |this, _event, _window, cx| {
            if this.control_state().recording == Some(id) {
                stop_recording(this);
                cx.notify();
            }
        }))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            if crate::keyboard::key(event) == Some(crate::keyboard::Key::Activate) && !event.is_held
            {
                let activation = if event.keystroke.key == "space" {
                    "space"
                } else {
                    "enter"
                };
                start_recording(this, id, Some(activation), on_record.clone(), window, cx);
                cx.stop_propagation();
            }
        }))
        .on_key_up(cx.listener(move |this, event: &KeyUpEvent, _window, _cx| {
            let state = this.control_state_mut();
            if state.recording == Some(id)
                && state.recording_activation == Some(event.keystroke.key.as_str())
            {
                state.recording_activation = None;
            }
        }))
        .child(label)
        .child(crate::keyboard::ring_for(&focus, CONTROL_RADIUS, palette))
}

type Record<V> = Rc<dyn Fn(&mut V, Chord, &mut Window, &mut Context<V>)>;

fn stop_recording<V: ControlHost>(view: &mut V) {
    let state = view.control_state_mut();
    state.recording = None;
    state.recording_keys = None;
    state.recording_activation = None;
}

fn start_recording<V: ControlHost>(
    view: &mut V,
    id: ComboId,
    activation: Option<&'static str>,
    on_record: Record<V>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let focus = view.control_state().focus(id, cx);
    window.focus(&focus, cx);
    let weak = cx.weak_entity();
    let focus_for_keys = focus.clone();
    // A bound shortcut becomes an action before `on_key_down` runs. Capture
    // it first, while this field holds focus, so recording cannot execute it.
    let keys = cx.intercept_keystrokes(move |event, window, cx| {
        if !focus_for_keys.is_focused(window) {
            return;
        }
        let _ = weak.update(cx, |this, cx| {
            if this.control_state().recording != Some(id) {
                return;
            }
            cx.stop_propagation();
            // The interceptor omits `is_held`. Ignore repeats of the key
            // that started recording until its release reaches the field.
            if this.control_state().recording_activation == Some(event.keystroke.key.as_str()) {
                return;
            }
            if event.keystroke.key == "escape" {
                stop_recording(this);
            } else if let Some(chord) = chord_for_keystroke(&event.keystroke) {
                stop_recording(this);
                on_record(this, chord, window, cx);
            }
            cx.notify();
        });
    });
    let blur = cx.on_blur(&focus, window, move |this, _window, cx| {
        if this.control_state().recording == Some(id) {
            stop_recording(this);
            cx.notify();
        }
    });
    let state = view.control_state_mut();
    state.recording = Some(id);
    state.recording_keys = Some([keys, blur]);
    state.recording_activation = activation;
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_chords_round_trip_every_modifier() {
        for binding in [
            "cmd-k",
            "ctrl-alt-shift-cmd-fn-k",
            "fn-left",
            "tab",
            "shift-tab",
        ] {
            let original = Keystroke::parse(binding).unwrap();
            let chord = chord_for_keystroke(&original).unwrap();
            assert_eq!(Keystroke::parse(&chord.keystroke).unwrap(), original);
            assert!(!chord.display.is_empty());
        }
        assert!(display("fn-left").contains("Fn"));
    }

    #[test]
    fn modifier_presses_do_not_finish_a_recording() {
        for key in ["", "cmd", "ctrl", "alt", "shift", "fn", "function"] {
            let mut stroke = Keystroke::parse("k").unwrap();
            stroke.key = key.into();
            assert_eq!(chord_for_keystroke(&stroke), None);
        }
    }
}
