//! Things that float above the view: tooltips and a command palette, and
//! the search field with results that the palette is built from.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, Context, Div, ElementId, Entity, FontWeight, InteractiveElement,
    MouseButton, MouseDownEvent, Render, SharedString, Window, deferred, div, prelude::*, px,
};

use crate::controls::{WidgetContext, search_field};
use crate::easing::{ease_out_cubic, progress};
use crate::keyboard::{self, Dismiss, Key, Orientation};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{COMBO_REVEAL, ComboId, ControlHost, MOVE, SWITCH_SLIDE, Tag};
use crate::text_input::{self as text, TextInput};

// ---- Tooltip ----------------------------------------------------------------

/// A tooltip's content: one line, optionally with an accelerator after it.
///
/// GPUI already owns the hard part, which is deciding when a tooltip should
/// appear and where it fits. This is only what it looks like, handed to
/// `.tooltip(..)` on any interactive element.
pub struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    palette: Palette,
}

/// What a tooltip says: a label, and optionally the shortcut after it.
///
/// A plain `&str` converts into one, so a control that takes a `Hint` can
/// be handed `"Undo"`; `Hint::new("Favourite").shortcut(display("secondary-d"))`
/// adds the accelerator. Purely a label: binding the key is the host's
/// business.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub text: SharedString,
    pub shortcut: Option<SharedString>,
}

impl Hint {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            shortcut: None,
        }
    }

    /// The accelerator shown after the label.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Builds the callback `.tooltip(..)` wants.
    pub fn tooltip(self, palette: Palette) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_window, cx| {
            let hint = self.clone();
            cx.new(move |_cx| Tooltip {
                text: hint.text,
                shortcut: hint.shortcut,
                palette,
            })
            .into()
        }
    }
}

impl From<&str> for Hint {
    fn from(text: &str) -> Self {
        Hint::new(text.to_string())
    }
}

impl From<String> for Hint {
    fn from(text: String) -> Self {
        Hint::new(text)
    }
}

impl From<SharedString> for Hint {
    fn from(text: SharedString) -> Self {
        Hint::new(text)
    }
}

impl Tooltip {
    /// Builds the callback `.tooltip(..)` wants, for any interactive element
    /// of the host's own. GPUI allows one tooltip per element.
    ///
    /// ```ignore
    /// div().id("thumbnail").child(image).tooltip(Tooltip::text("Open", palette))
    /// ```
    pub fn text(
        text: impl Into<SharedString>,
        palette: Palette,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Hint::new(text).tooltip(palette)
    }

    /// The same, with an accelerator shown after the label.
    pub fn with_shortcut(
        text: impl Into<SharedString>,
        shortcut: impl Into<SharedString>,
        palette: Palette,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Hint::new(text).shortcut(shortcut).tooltip(palette)
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let dark = palette.is_dark;
        let fill = if dark {
            palette.soft_fill
        } else {
            palette.field_surface
        };
        div()
            .px(px(8.0))
            .py(px(4.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(5.0))
            .bg(fill)
            .border_1()
            .border_color(lighting::rim(fill, dark))
            .shadow(lighting::panel(dark))
            .text_size(px(11.5))
            .text_color(palette.text_primary)
            .whitespace_nowrap()
            .child(self.text.clone())
            .children(
                self.shortcut
                    .clone()
                    .map(|shortcut| div().text_color(palette.text_secondary).child(shortcut)),
            )
    }
}

// ---- Command palette --------------------------------------------------------

/// One entry in a [`command_palette`].
#[derive(Clone, Debug)]
pub struct Command {
    /// Comes back to the activation callback.
    pub id: SharedString,
    pub label: SharedString,
    /// Where the command lives, shown quietly before the label.
    pub group: Option<SharedString>,
    pub shortcut: Option<SharedString>,
}

impl Command {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            group: None,
            shortcut: None,
        }
    }

    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }
}

/// How well a query matches a candidate, or `None` if it does not.
///
/// Subsequence matching, the way every fuzzy finder works: the query's
/// characters must appear in order, but not together. The score rewards
/// matches that start a word and matches that run on from the last one, so
/// "opf" ranks "Open File" above "Optional Prefix".
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.chars().all(char::is_whitespace) {
        return Some(0);
    }
    let candidate_lower = candidate.to_lowercase();
    let mut haystack = candidate_lower.chars().enumerate();
    let mut score = 0i32;
    let mut last_index: Option<usize> = None;
    let mut previous = None;

    for needle in query.to_lowercase().chars() {
        if needle.is_whitespace() {
            continue;
        }
        loop {
            let (index, ch) = haystack.next()?;
            let starts_word = previous.is_none_or(|previous: char| {
                previous.is_whitespace() || previous == '-' || previous == '_'
            });
            previous = Some(ch);
            if ch != needle {
                continue;
            }
            // Starting a word is the strongest signal that this is the
            // match the person meant.
            score += if starts_word { 12 } else { 2 };
            if last_index == Some(index.saturating_sub(1)) {
                score += 6;
            }
            last_index = Some(index);
            break;
        }
    }
    // Among equally good matches, the shortest candidate is the one that
    // most nearly *is* the query.
    Some(score - (candidate.chars().count() as i32 / 8))
}

/// Commands matching a query, best first. Ties keep their original order,
/// so a host's own ranking survives where the score cannot separate two.
pub fn fuzzy_filter(query: &str, commands: &[Command]) -> Vec<Command> {
    let mut scored: Vec<(i32, usize, Command)> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            let haystack = match &command.group {
                Some(group) => format!("{group} {}", command.label),
                None => command.label.to_string(),
            };
            fuzzy_score(query, &haystack).map(|score| (score, index, command.clone()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, command)| command).collect()
}

/// Height of one row in a [`command_list`].
pub const COMMAND_ROW_HEIGHT: f32 = 30.0;

/// A list of matching commands, one row each, with one highlighted.
///
/// The rows on their own, for a host that filters and moves the highlight
/// itself; [`search_list`] is these under a search field with the keys
/// wired, and [`command_palette`] is that in an overlay. Each row reports
/// its `Command::id` when clicked.
pub fn command_list<V: ControlHost>(
    id: &'static str,
    matches: &[Command],
    highlighted: usize,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Div {
    let WidgetContext { palette, view, cx } = ctx;
    // A concrete `Div` rather than `impl IntoElement`: in the 2024 edition an
    // opaque return captures every input lifetime, which would keep `cx`
    // borrowed for as long as the list lived and stop the caller building
    // anything else with it.
    let on_activate = Rc::new(on_activate);
    let state = view.control_state();
    // A list that has just appeared arrives whole, with whatever brought it.
    // Once it is up, rows the query lets through fade in at their place and
    // rows that stay slide to their new one, so typing reads as the list
    // being sifted rather than replaced.
    let fresh = !state.present(id);
    let mut rows: Vec<AnyElement> = Vec::with_capacity(matches.len());
    for (index, command) in matches.iter().enumerate() {
        let cx: &mut Context<V> = &mut *cx;
        let on_activate = on_activate.clone();
        let command_id = command.id.clone();
        let active = index == highlighted;
        let key = |what: &'static str| Tag::new((id, what, &command_id));
        // Keyed by the command rather than the row, so the highlight and the
        // fade belong to the command as it moves.
        let on = state.blend(key("lit"), active, SWITCH_SLIDE);
        // Secondary ink is for unselected rows; all selected text needs
        // the foreground paired with the accent surface underneath it.
        let secondary = crate::color::lerp(palette.text_secondary, palette.soft_label, on);
        let shown = if fresh {
            state.tween(key("shown"), 1.0, SWITCH_SLIDE)
        } else {
            state.tween_from(key("shown"), 0.0, 1.0, SWITCH_SLIDE)
        };
        let place = index as f32 * (COMMAND_ROW_HEIGHT + 1.0);
        let offset = state.tween(key("place"), place, MOVE) - place;
        rows.push(
            div()
                .id(ElementId::NamedInteger(
                    format!("{id}-command").into(),
                    index as u64,
                ))
                .relative()
                .top(px(offset))
                .opacity(shown)
                .h(px(COMMAND_ROW_HEIGHT))
                .flex_none()
                .w_full()
                .px(px(9.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(5.0))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(crate::color::lerp(
                    palette.text_primary,
                    palette.soft_label,
                    on,
                ))
                .when(on > 0.01, |el| {
                    el.bg(lighting::lit_at(palette.soft_fill, 0.08, on))
                })
                .when(!active, |el| {
                    el.hover(move |style| style.bg(palette.row_hover))
                })
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_activate(this, command_id.clone(), window, cx);
                    cx.notify();
                }))
                .children(
                    command
                        .group
                        .clone()
                        .map(|group| div().flex_none().text_color(secondary).child(group)),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(command.label.clone()),
                )
                .children(command.shortcut.clone().map(|shortcut| {
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(secondary)
                        .child(shortcut)
                }))
                .into_any_element(),
        );
    }
    // Clipped, so a row sliding up from where it was emerges from the list's
    // edge instead of crossing whatever sits below it.
    div()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .overflow_hidden()
        .children(rows)
}

type Activate<V> = Rc<dyn Fn(&mut V, SharedString, &mut Window, &mut Context<V>)>;

/// A search field with the rows that match it underneath, and the keys
/// between the two: Up and Down move the highlight, Enter picks it.
///
/// The host owns the query input and the items; this filters the items by
/// the query with [`fuzzy_filter`], keeps the keyboard's row in
/// [`ControlState`](crate::ControlState) under `id`, and reports the picked
/// `Command::id` — from a click and from Enter alike. The keys arrive as the
/// field's own actions: a single-line field has no use for Up, Down or an
/// Enter it has nothing to submit to, so it passes them on, and they are
/// caught here on the element that holds the field — which is also why they
/// only work while the field has the keyboard.
#[allow(clippy::too_many_arguments)]
pub fn search_list<V: ControlHost>(
    id: &'static str,
    query: &Entity<TextInput>,
    items: &[Command],
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Div {
    let palette = ctx.palette;
    let view = ctx.view;
    let on_activate: Activate<V> = Rc::new(on_activate);
    let text = query.read(ctx.cx).text();
    let matches = fuzzy_filter(&text, items);
    let highlighted = view
        .control_state()
        .list_highlight(id, &text)
        .min(matches.len().saturating_sub(1));
    let rows = {
        let on_activate = on_activate.clone();
        command_list(
            id,
            &matches,
            highlighted,
            ctx.reborrow(),
            move |view, id, window, cx| on_activate(view, id, window, cx),
        )
    };
    let cx: &mut Context<V> = &mut *ctx.cx;
    let ids = matches.iter().map(|command| command.id.clone()).collect();
    let list = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(search_field(id, query, palette, window, cx))
        .child(rows);
    list_keys(list, id, ids, cx, on_activate)
}

/// Puts a filtering list's keys on the element that holds its query field:
/// Up and Down move the highlight through `ids`, Enter picks the
/// highlighted one.
fn list_keys<V: ControlHost, E: InteractiveElement>(
    element: E,
    id: &'static str,
    ids: Vec<SharedString>,
    cx: &mut Context<V>,
    on_activate: Activate<V>,
) -> E {
    let count = ids.len();
    let step = move |this: &mut V, key: Key, cx: &mut Context<V>| {
        let state = this.control_state();
        let here = state.list_position(id).min(count.saturating_sub(1));
        if let Some(moved) = keyboard::step(key, Orientation::Vertical, here, count) {
            state.highlight_list(id, moved);
            cx.notify();
        }
    };
    let step_down = step;
    element
        .on_action(cx.listener(move |this, _: &text::Up, _window, cx| {
            step(this, Key::Up, cx);
        }))
        .on_action(cx.listener(move |this, _: &text::Down, _window, cx| {
            step_down(this, Key::Down, cx);
        }))
        .on_action(cx.listener(move |this, _: &text::Enter, window, cx| {
            let here = this
                .control_state()
                .list_position(id)
                .min(count.saturating_sub(1));
            if let Some(picked) = ids.get(here) {
                on_activate(this, picked.clone(), window, cx);
                cx.notify();
            }
        }))
}

/// Centred overlay with a filter field and a list of matching commands.
///
/// Open it with [`ControlState::open_palette`](crate::ControlState::open_palette)
/// — or [`toggle_palette`](crate::ControlState::toggle_palette), from
/// whatever shortcut the host gives it; while `id` is not open this returns
/// nothing. The host owns the query input and the commands. The palette
/// filters them with [`fuzzy_filter`], moves the highlight with Up and Down,
/// runs the highlighted command on Enter or a clicked one, and closes on
/// Escape or a press on the scrim. It closes before a command runs, handing
/// the keyboard back where it was, so a command that opens something starts
/// from the right place.
#[allow(clippy::too_many_arguments)]
pub fn command_palette<V: ControlHost>(
    id: ComboId,
    query: &Entity<TextInput>,
    commands: &[Command],
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Option<impl IntoElement> {
    let palette = ctx.palette;
    let view = ctx.view;
    let state = view.control_state();
    let opened_at = state
        .palette_overlay
        .as_ref()
        .filter(|open| open.id == id)?
        .opened_at;
    let dark = palette.is_dark;
    let on_activate: Activate<V> = Rc::new(move |view: &mut V, picked, window, cx| {
        view.control_state_mut().close_palette(window, cx);
        on_activate(view, picked, window, cx);
    });
    // The panel arrives the way a menu does: the same reveal, a short drift
    // down into place.
    let reveal = ease_out_cubic(progress(opened_at, state.scaled(COMBO_REVEAL)));
    let list_scroll = state.scroll((id, "list"));
    let fill = if dark {
        palette.soft_fill
    } else {
        palette.field_surface
    };

    let text = query.read(ctx.cx).text();
    let matches = fuzzy_filter(&text, commands);
    let highlighted = state
        .list_highlight(id, &text)
        .min(matches.len().saturating_sub(1));
    let empty = matches.is_empty();
    let rows = {
        let on_activate = on_activate.clone();
        command_list(
            id,
            &matches,
            highlighted,
            ctx.reborrow(),
            move |view, id, window, cx| on_activate(view, id, window, cx),
        )
    };
    let cx: &mut Context<V> = &mut *ctx.cx;
    let ids = matches.iter().map(|command| command.id.clone()).collect();

    let panel = div()
        // A little above centre: the list grows downward, and a palette
        // pinned to the middle ends up low on the screen as soon as it has
        // results.
        .mt(gpui::relative(0.16))
        .relative()
        .top(px(-6.0 * (1.0 - reveal)))
        .opacity(reveal)
        .w(px(520.0))
        .max_w(gpui::relative(0.9))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(8.0))
        .rounded(px(10.0))
        .bg(fill)
        .border_1()
        .border_color(lighting::rim(fill, dark))
        .shadow(lighting::panel(dark))
        .occlude()
        // Once `bind_keys` has run, Escape arrives as this action and never
        // as a key. The query field has the keyboard, and this is the first
        // thing above it.
        .on_action(cx.listener(move |this, _: &Dismiss, window, cx| {
            this.control_state_mut().close_palette(window, cx);
            cx.notify();
        }))
        .child(search_field(id, query, palette, window, cx))
        .child(
            div()
                .relative()
                .child(
                    div()
                        .id(ElementId::Name(format!("{id}-list").into()))
                        .max_h(px(340.0))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .overflow_y_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&list_scroll)
                        .child(rows)
                        .when(empty, |el| {
                            el.child(
                                div()
                                    .h(px(30.0))
                                    .px(px(9.0))
                                    .flex()
                                    .items_center()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(palette.text_secondary)
                                    .child("No matching commands"),
                            )
                        }),
                )
                // A long list fades at the edge that has more past it,
                // rather than slicing a row in half.
                .children(crate::scroll::scroll_fades(
                    state,
                    id,
                    &list_scroll,
                    crate::scroll::ScrollAxis::Vertical,
                    fill,
                    fill,
                )),
        );
    let panel = list_keys(panel, id, ids, cx, on_activate);

    Some(
        deferred(
            div()
                .id(ElementId::Name(format!("{id}-scrim").into()))
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .bg(crate::color::with_alpha(
                    palette.scrim(),
                    palette.scrim().alpha * reveal,
                ))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                        this.control_state_mut().close_palette(window, cx);
                        cx.notify();
                    }),
                )
                .child(panel),
        )
        .with_priority(180),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_starts_outrank_scattered_letters() {
        let open_file = fuzzy_score("opf", "Open File").expect("matches");
        let scattered = fuzzy_score("opf", "Optional Prefix").expect("matches");
        assert!(
            open_file > scattered,
            "expected Open File ({open_file}) above Optional Prefix ({scattered})"
        );
    }

    #[test]
    fn a_query_that_is_not_a_subsequence_does_not_match() {
        assert!(fuzzy_score("zzz", "Open File").is_none());
        assert!(fuzzy_score("elif", "Open File").is_none());
    }

    #[test]
    fn an_empty_query_keeps_everything_in_its_original_order() {
        let commands = vec![
            Command::new("b", "Much longer first command"),
            Command::new("a", "First"),
        ];
        for query in ["", " \t\n"] {
            let filtered = fuzzy_filter(query, &commands);
            let ids: Vec<&str> = filtered.iter().map(|command| command.id.as_ref()).collect();
            assert_eq!(ids, vec!["b", "a"]);
        }
    }

    #[test]
    fn consecutive_unicode_characters_get_the_same_bonus_as_ascii() {
        assert_eq!(fuzzy_score("été", "été"), fuzzy_score("ete", "ete"));
        assert_eq!(fuzzy_score("b", "a\tb"), fuzzy_score("b", "a b"));
    }

    #[test]
    fn the_group_is_searched_along_with_the_label() {
        let commands = vec![Command::new("save", "Save").group("File")];
        assert_eq!(fuzzy_filter("file sa", &commands).len(), 1);
    }
}
