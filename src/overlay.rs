//! Things that float above the view: tooltips and a command palette.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, Context, Div, ElementId, Entity, FontWeight, MouseButton,
    MouseDownEvent, Render, SharedString, Window, deferred, div, prelude::*, px,
};

use crate::controls::search_field;
use crate::lighting;
use crate::palette::Palette;
use crate::state::{ControlHost, MOVE, SWITCH_SLIDE};
use crate::text_input::TextInput;

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

impl Tooltip {
    /// Builds the callback `.tooltip(..)` wants.
    ///
    /// ```ignore
    /// controls::icon_button("undo", glyph, 26.0, false, true, &palette, cx, ..)
    ///     .tooltip(Tooltip::text("Undo", palette))
    /// ```
    pub fn text(
        text: impl Into<SharedString>,
        palette: Palette,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        move |_window, cx| {
            let text = text.clone();
            cx.new(move |_cx| Tooltip {
                text,
                shortcut: None,
                palette,
            })
            .into()
        }
    }

    /// The same, with an accelerator shown after the label. Purely a label:
    /// binding the key is the host's business.
    pub fn with_shortcut(
        text: impl Into<SharedString>,
        shortcut: impl Into<SharedString>,
        palette: Palette,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        let shortcut = shortcut.into();
        move |_window, cx| {
            let (text, shortcut) = (text.clone(), shortcut.clone());
            cx.new(move |_cx| Tooltip {
                text,
                shortcut: Some(shortcut),
                palette,
            })
            .into()
        }
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
    if query.is_empty() {
        return Some(0);
    }
    let candidate_lower = candidate.to_lowercase();
    let mut haystack = candidate_lower.char_indices().peekable();
    let mut score = 0i32;
    let mut last_index: Option<usize> = None;

    for needle in query.to_lowercase().chars() {
        if needle.is_whitespace() {
            continue;
        }
        loop {
            let (index, ch) = haystack.next()?;
            if ch != needle {
                continue;
            }
            // Starting a word is the strongest signal that this is the
            // match the person meant.
            let starts_word = index == 0
                || candidate_lower[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|previous| previous == ' ' || previous == '-' || previous == '_');
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
    Some(score - (candidate.len() as i32 / 8))
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
/// The rows the command palette shows, on their own — for a search field
/// with its results underneath, an "open recent" list, anywhere a query has
/// a list of answers. The host owns the matches and the highlight, because
/// those are what its arrow keys move and what its Enter commits;
/// [`fuzzy_filter`] does the ranking. Each row reports its `Command::id`
/// when clicked.
pub fn command_list<V: ControlHost>(
    id: &'static str,
    matches: &[Command],
    highlighted: usize,
    palette: Palette,
    view: &V,
    cx: &mut Context<V>,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Div {
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
        let key = |what: &str| ElementId::Name(format!("{id}-{what}-{command_id}").into());
        // Keyed by the command rather than the row, so the highlight and the
        // fade belong to the command as it moves.
        let on = state.blend(&key("lit"), active, SWITCH_SLIDE);
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
                    palette.control_label,
                    on,
                ))
                .when(on > 0.01, |el| {
                    el.bg(lighting::lit_at(palette.control_fill, 0.08, on))
                })
                .when(!active, |el| {
                    el.hover(move |style| style.bg(palette.row_hover))
                })
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_activate(this, command_id.clone(), window, cx);
                    cx.notify();
                }))
                .children(command.group.clone().map(|group| {
                    div()
                        .flex_none()
                        .text_color(palette.text_secondary)
                        .child(group)
                }))
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
                        .text_color(palette.text_secondary)
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

/// Centred overlay with a filter field and a list of matching commands.
///
/// The host owns the query input, the filtered list and the highlighted
/// index, because all three are also what the up and down keys move and
/// what Enter commits, and those bindings belong with the host's other
/// keys. [`fuzzy_filter`] does the ranking.
#[allow(clippy::too_many_arguments)]
pub fn command_palette<V: ControlHost>(
    id: &'static str,
    query: &Entity<TextInput>,
    matches: &[Command],
    highlighted: usize,
    palette: Palette,
    view: &V,
    window: &Window,
    cx: &mut Context<V>,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
    on_dismiss: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let dark = palette.is_dark;
    let on_activate = Rc::new(on_activate);
    let list_scroll = view
        .control_state()
        .scroll(ElementId::Name(format!("{id}-list").into()));
    let fill = if dark {
        palette.soft_fill
    } else {
        palette.field_surface
    };

    let empty = matches.is_empty();
    let rows = {
        let on_activate = on_activate.clone();
        command_list(
            id,
            matches,
            highlighted,
            palette,
            view,
            cx,
            move |view, id, window, cx| on_activate(view, id, window, cx),
        )
    };

    deferred(
        div()
            .id(ElementId::Name(format!("{id}-scrim").into()))
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .bg(palette.scrim())
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    on_dismiss(this, window, cx);
                    cx.notify();
                }),
            )
            .child(
                div()
                    // A little above centre: the list grows downward, and a
                    // palette pinned to the middle ends up low on the screen
                    // as soon as it has results.
                    .mt(gpui::relative(0.16))
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
                            // A long list fades at the edge that has more
                            // past it, rather than slicing a row in half.
                            .children(crate::scroll::scroll_fades(
                                view.control_state(),
                                id,
                                &list_scroll,
                                crate::scroll::ScrollAxis::Vertical,
                                fill,
                                fill,
                            )),
                    ),
            ),
    )
    .with_priority(180)
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
        let commands = vec![Command::new("b", "Second"), Command::new("a", "First")];
        let filtered = fuzzy_filter("", &commands);
        let ids: Vec<&str> = filtered.iter().map(|command| command.id.as_ref()).collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn the_group_is_searched_along_with_the_label() {
        let commands = vec![Command::new("save", "Save").group("File")];
        assert_eq!(fuzzy_filter("file sa", &commands).len(), 1);
    }
}
