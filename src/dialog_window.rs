//! A prompt, a confirmation or an alert in a small window of its own.
//!
//! The in-window [`dialog`](crate::dialog) is the one to reach for: it sits
//! over the view it belongs to and fades with it. A host whose window has a
//! live native view over it — a web view, a video player — cannot use it
//! there, because the view is composited over everything GPUI draws; so
//! these open a GPUI dialog window instead, which floats above the native
//! view like any other window.
//!
//! ```ignore
//! dialog_window::prompt(cx, palette, "Rename", "", "Untitled", "Rename",
//!     move |answer, cx| if let Some(name) = answer { rename(name, cx) });
//! dialog_window::confirm(cx, palette, "Clear history?", "This cannot be undone.",
//!     "Clear", move |accepted, cx| if accepted { clear(cx) });
//! dialog_window::alert(cx, palette, "Couldn't save", &error);
//! ```
//!
//! The window is as tall as what it holds, however long the message runs.
//! Enter submits, Escape cancels, and a prompt's field has the keyboard
//! from the start. The answer arrives through the callback with the `App`,
//! from which a host reaches its own entity.

use gpui::{
    App, Context, Entity, FontWeight, Render, Window, WindowBounds, WindowKind, WindowOptions,
    canvas, div, prelude::*, px, size,
};

use crate::containers::DialogButton;
use crate::controls::{ButtonVariant, text_field};
use crate::keyboard::{self, Dismiss};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{ControlHost, ControlState};
use crate::text_input::InputStyle;
use crate::text_input::{self, TextInput};

const WIDTH: f32 = 420.0;
const PADDING: f32 = 20.0;
const GAP: f32 = 14.0;
const TITLE_HEIGHT: f32 = 22.0;
const LINE_HEIGHT: f32 = 18.0;
const FIELD_HEIGHT: f32 = 32.0;
const BUTTON_HEIGHT: f32 = 32.0;
/// About how many characters of the message fit a line, for the first
/// guess at the height.
const CHARS_PER_LINE: f32 = 55.0;

/// A first guess at the height the dialog needs, close enough that fitting
/// it to what it really holds barely moves it: the title, the message at
/// about [`CHARS_PER_LINE`] a line, the field, the buttons.
fn estimated_height(message: &str, field: bool) -> f32 {
    let lines = message
        .split('\n')
        .map(|line| {
            (line.chars().count() as f32 / CHARS_PER_LINE)
                .ceil()
                .max(1.0)
        })
        .sum::<f32>();
    let message = if message.is_empty() {
        0.0
    } else {
        lines * LINE_HEIGHT + GAP
    };
    let field = if field { FIELD_HEIGHT + GAP } else { 0.0 };
    2.0 * PADDING + TITLE_HEIGHT + GAP + message + field + BUTTON_HEIGHT
}

/// What a dialog asks for.
enum Kind {
    Prompt(String),
    Confirm,
    Alert,
}

type Answer = Box<dyn FnOnce(Option<String>, &mut App)>;

struct Dialog {
    controls: ControlState,
    palette: Palette,
    title: String,
    message: String,
    button: String,
    alert: bool,
    input: Option<Entity<TextInput>>,
    answer: Option<Answer>,
}

impl ControlHost for Dialog {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }
    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }
}

impl Dialog {
    fn finish(&mut self, accepted: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answer) = self.answer.take() else {
            return;
        };
        let text = accepted.then(|| {
            self.input
                .as_ref()
                .map_or_else(String::new, |input| input.read(cx).text())
        });
        window.remove_window();
        answer(text, cx);
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let input = self.input.clone();
        let title = self.title.clone();
        let message = self.message.clone();
        let alert = self.alert;
        let mut buttons: Vec<DialogButton<Dialog>> = Vec::with_capacity(2);
        if !alert {
            buttons.push(DialogButton::new(
                "dialog-cancel",
                "Cancel",
                ButtonVariant::Soft,
                |this: &mut Dialog, window, cx| this.finish(false, window, cx),
            ));
        }
        buttons.push(DialogButton::new(
            "dialog-submit",
            self.button.clone(),
            ButtonVariant::Primary,
            |this: &mut Dialog, window, cx| this.finish(true, window, cx),
        ));
        let footer = div()
            .flex()
            .justify_end()
            .gap(px(8.0))
            .children(buttons.into_iter().map(|spec| {
                let on_click = spec.on_click;
                div()
                    .min_w(px(110.0))
                    .flex_none()
                    .child(crate::controls::button(
                        spec.id,
                        &spec.label,
                        spec.variant,
                        spec.enabled,
                        palette,
                        cx,
                        move |this, window, cx| on_click(this, window, cx),
                    ))
            }));
        // The window takes the height of what it holds, however long the
        // message runs, rather than a fixed one that cut the buttons off.
        let fit = canvas(
            |bounds, window, _| {
                let wanted = f32::from(bounds.size.height) + 2.0 * PADDING;
                if (f32::from(window.viewport_size().height) - wanted).abs() > 0.5 {
                    window.resize(size(px(WIDTH), px(wanted)));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        keyboard::root(div().id("dialog-window"), self, cx)
            .size_full()
            .p(px(PADDING))
            .flex()
            .flex_col()
            .font_family(crate::typography::ui_font())
            .text_size(px(13.0))
            .text_color(palette.text_primary)
            .bg(palette.field_surface)
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                this.finish(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &text_input::Enter, window, cx| {
                this.finish(true, window, cx);
            }))
            // Enter is only an action inside a field; a confirmation or an
            // alert has none, so the key itself submits there.
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if keyboard::key(event) == Some(keyboard::Key::Activate) {
                    cx.stop_propagation();
                    this.finish(true, window, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(GAP))
                    .child(fit)
                    .child(
                        div()
                            .text_size(px(17.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(!message.is_empty(), |el| {
                        el.child(div().text_color(palette.text_secondary).child(message))
                    })
                    .children(
                        input
                            .as_ref()
                            .map(|input| text_field(input, palette, window, cx)),
                    )
                    .child(footer),
            )
            .shadow(lighting::panel(palette.is_dark))
    }
}

/// Asks for a line of text, `initial` already in the field, under `title`
/// and `message`; `button` names the way to say yes. Answers with the
/// text, or `None` when cancelled.
pub fn prompt(
    cx: &mut App,
    palette: Palette,
    title: impl Into<String>,
    message: impl Into<String>,
    initial: impl Into<String>,
    button: impl Into<String>,
    on_answer: impl FnOnce(Option<String>, &mut App) + 'static,
) {
    open(
        cx,
        palette,
        title.into(),
        message.into(),
        button.into(),
        Kind::Prompt(initial.into()),
        Box::new(on_answer),
    );
}

/// Asks whether to go ahead; `button` names going ahead. Answers true
/// for it, false for Cancel or Escape.
pub fn confirm(
    cx: &mut App,
    palette: Palette,
    title: impl Into<String>,
    message: impl Into<String>,
    button: impl Into<String>,
    on_answer: impl FnOnce(bool, &mut App) + 'static,
) {
    open(
        cx,
        palette,
        title.into(),
        message.into(),
        button.into(),
        Kind::Confirm,
        Box::new(move |answer, cx| on_answer(answer.is_some(), cx)),
    );
}

/// Tells the reader something, with one button to put it away.
pub fn alert(cx: &mut App, palette: Palette, title: impl Into<String>, message: impl Into<String>) {
    open(
        cx,
        palette,
        title.into(),
        message.into(),
        "OK".into(),
        Kind::Alert,
        Box::new(|_, _| {}),
    );
}

fn open(
    cx: &mut App,
    palette: Palette,
    title: String,
    message: String,
    button: String,
    kind: Kind,
    answer: Answer,
) {
    let alert = matches!(kind, Kind::Alert);
    let input = match kind {
        Kind::Prompt(initial) => Some(cx.new(|cx| {
            let mut input = TextInput::new(cx, "", false, InputStyle::from_palette(palette, 13.0));
            input.set_text(&initial, cx);
            input
        })),
        Kind::Confirm | Kind::Alert => None,
    };
    let height = estimated_height(&message, input.is_some());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(WIDTH), px(height)), cx)),
        kind: WindowKind::Dialog,
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let focus = input
        .as_ref()
        .map(|input| input.read(cx).focus_handle.clone());
    // A window that cannot open answers as cancelled, so the caller is
    // never left waiting.
    let mut answer = Some(answer);
    let result = cx.open_window(options, |window, cx| {
        let controls = ControlState::default();
        // The field has the keyboard from the start; without one, the
        // dialog's root does, so Enter and Escape still reach it.
        match &focus {
            Some(focus) => window.focus(focus, cx),
            None => controls.focus_root(window, cx),
        }
        cx.new(|_| Dialog {
            controls,
            palette,
            title,
            message,
            button,
            alert,
            input,
            answer: answer.take(),
        })
    });
    if result.is_err()
        && let Some(answer) = answer
    {
        answer(None, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guess only has to land near the fitted height: a short message
    /// is one line, a long one wraps, and a field adds its own row.
    #[test]
    fn the_first_guess_counts_lines_and_rows() {
        let chrome = 2.0 * PADDING + TITLE_HEIGHT + GAP + BUTTON_HEIGHT;
        assert_eq!(estimated_height("", false), chrome);
        assert_eq!(estimated_height("Short", false), chrome + LINE_HEIGHT + GAP);
        let long = "a".repeat(120);
        assert_eq!(
            estimated_height(&long, false),
            chrome + 3.0 * LINE_HEIGHT + GAP
        );
        assert_eq!(
            estimated_height("One\nTwo", true),
            chrome + 2.0 * LINE_HEIGHT + GAP + FIELD_HEIGHT + GAP
        );
    }
}
