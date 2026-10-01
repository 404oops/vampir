//! Things that float above the view: tooltips and a command palette, and
//! the search field with results that the palette is built from.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, Context, Div, ElementId, Entity, FontWeight, InteractiveElement,
    KeyDownEvent, MouseButton, MouseDownEvent, Render, ScrollHandle, SharedString, StyledText,
    Window, deferred, div, point, prelude::*, px,
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

/// One result from a host-ranked search. Its position is already its rank;
/// [`ranked_search_list`] and [`ranked_command_palette`] never filter or sort
/// it. The host may replace the slice when another source finishes loading.
/// Result ids must be unique within one list and stable across updates.
#[derive(Clone)]
pub struct SearchResult {
    pub id: SharedString,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    /// Shown once before each run of results from the same source.
    pub section: Option<SharedString>,
    pub shortcut: Option<SharedString>,
    leading: Option<Rc<dyn Fn(Palette) -> AnyElement>>,
}

impl SearchResult {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            section: None,
            shortcut: None,
            leading: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn section(mut self, section: impl Into<SharedString>) -> Self {
        self.section = Some(section.into());
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// A palette-aware visual in the fixed leading slot, such as an icon or
    /// favicon. The host supplies its own drawing; the toolkit sizes and
    /// aligns the slot across the list.
    pub fn leading<E: IntoElement + 'static>(
        mut self,
        render: impl Fn(Palette) -> E + 'static,
    ) -> Self {
        self.leading = Some(Rc::new(move |palette| render(palette).into_any_element()));
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

/// Byte ranges to emphasize in displayed text. A whole word match reads
/// best; when the host matched a fuzzy abbreviation, mark its letters.
fn matched_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let mut ranges = Vec::new();
    for word in query.split_whitespace() {
        let needle: Vec<char> = word.chars().collect();
        let positions = (0..=chars.len().saturating_sub(needle.len()))
            .find(|&start| {
                start + needle.len() <= chars.len()
                    && needle
                        .iter()
                        .enumerate()
                        .all(|(offset, &ch)| same(chars[start + offset].1, ch))
            })
            .map(|start| (start..start + needle.len()).collect::<Vec<_>>())
            .or_else(|| {
                let mut found = Vec::new();
                let mut from = 0;
                for &ch in &needle {
                    let index = (from..chars.len()).find(|&i| same(chars[i].1, ch))?;
                    found.push(index);
                    from = index + 1;
                }
                Some(found)
            });
        if let Some(positions) = positions {
            for index in positions {
                let start = chars[index].0;
                let end = chars.get(index + 1).map_or(text.len(), |next| next.0);
                ranges.push(start..end);
            }
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn emphasized(text: &SharedString, query: &str) -> StyledText {
    StyledText::new(text.to_string()).with_highlights(
        matched_ranges(text, query)
            .into_iter()
            .map(|range| (range, FontWeight::SEMIBOLD.into())),
    )
}

/// Renders a host-ranked set of results in the order supplied. Section names
/// are quiet headings rather than selectable rows; only results count when
/// Up, Down and Enter move the highlight.
struct RankedRows {
    elements: Vec<AnyElement>,
    child_indices: Vec<usize>,
    positions: Vec<f32>,
}

struct RowPlacement {
    heading: bool,
    child_index: usize,
    top: f32,
    height: f32,
}

fn ranked_row_layout(results: &[SearchResult]) -> Vec<RowPlacement> {
    let mut previous_section: Option<&str> = None;
    let mut children = 0;
    let mut top = 0.0;
    results
        .iter()
        .map(|result| {
            let section = result.section.as_deref();
            let heading = section.is_some() && section != previous_section;
            if heading {
                children += 1;
                top += 21.0;
            }
            previous_section = section;
            let height = if result.detail.is_some() { 44.0 } else { 32.0 };
            let placement = RowPlacement {
                heading,
                child_index: children,
                top,
                height,
            };
            children += 1;
            top += height + 1.0;
            placement
        })
        .collect()
}

fn ranked_result_rows<V: ControlHost>(
    id: &'static str,
    query: &str,
    results: &[SearchResult],
    highlighted: usize,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_activate: Activate<V>,
) -> RankedRows {
    let WidgetContext { palette, view, cx } = ctx;
    let state = view.control_state();
    let fresh = !state.present(id);
    let reduce_motion = cx.reduce_motion();
    let has_leading = results.iter().any(|result| result.leading.is_some());
    let placements = ranked_row_layout(results);
    let mut rows: Vec<AnyElement> = Vec::with_capacity(results.len() * 2);
    let mut child_indices = Vec::with_capacity(results.len());
    let mut positions = Vec::with_capacity(results.len());
    for (index, (result, placement)) in results.iter().zip(&placements).enumerate() {
        let section = result.section.as_deref();
        if placement.heading
            && let Some(section) = section
        {
            let key = (id, "section-shown", &result.id);
            let shown = if reduce_motion {
                state.snap(key, 1.0)
            } else if fresh {
                state.tween(key, 1.0, SWITCH_SLIDE)
            } else {
                state.tween_from(key, 0.0, 1.0, SWITCH_SLIDE)
            };
            rows.push(
                div()
                    .h(px(20.0))
                    .flex_none()
                    .px(px(if has_leading { 37.0 } else { 9.0 }))
                    .flex()
                    .items_center()
                    .opacity(shown)
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.text_secondary)
                    .child(section.to_string())
                    .into_any_element(),
            );
        }
        let cx: &mut Context<V> = &mut *cx;
        let command_id = result.id.clone();
        let active = index == highlighted;
        let key = |part: &'static str| Tag::new((id, part, &command_id));
        let on = if reduce_motion {
            state.snap(key("lit"), if active { 1.0 } else { 0.0 })
        } else {
            state.blend(key("lit"), active, SWITCH_SLIDE)
        };
        let shown = if reduce_motion {
            state.snap(key("shown"), 1.0)
        } else if fresh {
            state.tween(key("shown"), 1.0, SWITCH_SLIDE)
        } else {
            state.tween_from(key("shown"), 0.0, 1.0, SWITCH_SLIDE)
        };
        let offset = if reduce_motion {
            state.snap(key("place"), placement.top);
            0.0
        } else {
            state.tween(key("place"), placement.top, MOVE) - placement.top
        };
        let secondary = crate::color::lerp(palette.text_secondary, palette.soft_label, on);
        let leading = result.leading.as_ref().map(|render| render(palette));
        let on_activate = on_activate.clone();
        child_indices.push(placement.child_index);
        positions.push(placement.top);
        rows.push(
            div()
                .id(ElementId::Name(format!("{id}-result-{}", result.id).into()))
                .relative()
                .top(px(offset))
                .opacity(shown)
                .h(px(placement.height))
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
                .when(has_leading, |el| {
                    el.child(
                        div()
                            .size(px(20.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .children(leading),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().truncate().child(emphasized(&result.label, query)))
                        .children(result.detail.as_ref().map(|detail| {
                            div()
                                .truncate()
                                .text_size(px(11.5))
                                .text_color(secondary)
                                .child(emphasized(detail, query))
                        })),
                )
                .children(result.shortcut.clone().map(|shortcut| {
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(secondary)
                        .child(shortcut)
                }))
                .into_any_element(),
        );
    }
    RankedRows {
        elements: rows,
        child_indices,
        positions,
    }
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
        .list_highlight(id, text.as_str())
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
    list_keys(list, id, ids, None, cx, on_activate)
}

/// A search field over results already filtered and ranked by the host.
/// Source updates may replace `results` at any time; a changed query or
/// result order resets the keyboard highlight to the first result. Section
/// headings do not take a keyboard step. The supplied `id` identifies this
/// list in [`ControlState`](crate::ControlState), just as for [`search_list`].
/// Long result sets scroll within the list, leaving the query field in place.
#[allow(clippy::too_many_arguments)]
pub fn ranked_search_list<V: ControlHost>(
    id: &'static str,
    query: &Entity<TextInput>,
    results: &[SearchResult],
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Div {
    let palette = ctx.palette;
    let text = query.read(ctx.cx).text();
    let ids: Vec<SharedString> = results.iter().map(|result| result.id.clone()).collect();
    let (highlighted, changed) = ctx
        .view
        .control_state()
        .list_highlight_with_change(id, (text.as_str(), ids.as_slice()));
    let highlighted = highlighted.min(results.len().saturating_sub(1));
    let list_scroll = ctx.view.control_state().scroll((id, "list"));
    if changed {
        list_scroll.set_offset(Default::default());
    }
    let on_activate: Activate<V> = Rc::new(on_activate);
    let rows = ranked_result_rows(
        id,
        &text,
        results,
        highlighted,
        ctx.reborrow(),
        on_activate.clone(),
    );
    let navigation = Rc::new(ResultNavigation {
        scroll: list_scroll.clone(),
        child_indices: rows.child_indices,
        positions: rows.positions,
    });
    let state = ctx.view.control_state();
    let cx: &mut Context<V> = &mut *ctx.cx;
    let list = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(search_field(id, query, palette, window, cx))
        .child(
            div()
                .relative()
                .child(
                    div()
                        .id(ElementId::Name(format!("{id}-list").into()))
                        .max_h(px(INLINE_RESULT_MAX_HEIGHT))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .overflow_y_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&list_scroll)
                        .children(rows.elements)
                        .when(results.is_empty(), |el| {
                            el.child(
                                div()
                                    .h(px(30.0))
                                    .px(px(9.0))
                                    .flex()
                                    .items_center()
                                    .text_size(px(12.5))
                                    .text_color(palette.text_secondary)
                                    .child("No results"),
                            )
                        }),
                )
                .children(crate::scroll::scroll_fades(
                    state,
                    id,
                    &list_scroll,
                    crate::scroll::ScrollAxis::Vertical,
                    palette.area_surface,
                    palette.area_surface,
                )),
        );
    list_keys(list, id, ids, Some(navigation), cx, on_activate)
}

const INLINE_RESULT_MAX_HEIGHT: f32 = 300.0;

struct ResultNavigation {
    scroll: ScrollHandle,
    child_indices: Vec<usize>,
    positions: Vec<f32>,
}

fn page_target(key: Key, current: usize, positions: &[f32], viewport: f32) -> Option<usize> {
    let last = positions.len().checked_sub(1)?;
    let current = current.min(last);
    let page = viewport.max(1.0);
    match key {
        Key::PageDown => {
            let threshold = positions[current] + page;
            Some(
                ((current + 1)..=last)
                    .find(|&index| positions[index] >= threshold)
                    .unwrap_or(last),
            )
        }
        Key::PageUp => {
            let threshold = positions[current] - page;
            Some(
                (0..current)
                    .rev()
                    .find(|&index| positions[index] <= threshold)
                    .unwrap_or(0),
            )
        }
        _ => None,
    }
}

fn revealed_offset(offset: f32, max_offset: f32, viewport: Range<f32>, item: Range<f32>) -> f32 {
    let margin = crate::scroll::SCROLL_FADE.min((viewport.end - viewport.start) / 4.0);
    let wanted = if item.start + offset < viewport.start + margin {
        viewport.start + margin - item.start
    } else if item.end + offset > viewport.end - margin {
        viewport.end - margin - item.end
    } else {
        offset
    };
    wanted.clamp(-max_offset.max(0.0), 0.0)
}

fn reveal_result(navigation: &ResultNavigation, index: usize) {
    let scroll = &navigation.scroll;
    let Some(item) = scroll.bounds_for_item(navigation.child_indices[index]) else {
        return;
    };
    let viewport = scroll.bounds();
    let offset = scroll.offset();
    let next = revealed_offset(
        f32::from(offset.y),
        f32::from(scroll.max_offset().y),
        f32::from(viewport.top())..f32::from(viewport.bottom()),
        f32::from(item.top())..f32::from(item.bottom()),
    );
    scroll.set_offset(point(offset.x, px(next)));
}

fn navigate_list<V: ControlHost>(
    this: &mut V,
    id: &'static str,
    count: usize,
    key: Key,
    navigation: Option<&ResultNavigation>,
    cx: &mut Context<V>,
) {
    let state = this.control_state();
    let here = state.list_position(id).min(count.saturating_sub(1));
    let moved = match (key, navigation) {
        (Key::PageUp | Key::PageDown, Some(navigation)) => page_target(
            key,
            here,
            &navigation.positions,
            f32::from(navigation.scroll.bounds().size.height),
        ),
        (Key::PageUp, None) => count.checked_sub(1).map(|_| here.saturating_sub(10)),
        (Key::PageDown, None) => count.checked_sub(1).map(|last| (here + 10).min(last)),
        _ => keyboard::step(key, Orientation::Vertical, here, count),
    };
    if let Some(moved) = moved {
        state.highlight_list(id, moved);
        if let Some(navigation) = navigation {
            reveal_result(navigation, moved);
        }
        cx.notify();
    }
}

/// Puts a filtering list's keys on the element that holds its query field:
/// Up, Down and Page move the highlight through `ids`, Enter picks it.
fn list_keys<V: ControlHost, E: InteractiveElement>(
    element: E,
    id: &'static str,
    ids: Vec<SharedString>,
    navigation: Option<Rc<ResultNavigation>>,
    cx: &mut Context<V>,
    on_activate: Activate<V>,
) -> E {
    let count = ids.len();
    let up_navigation = navigation.clone();
    let down_navigation = navigation.clone();
    let page_navigation = navigation;
    element
        .on_action(cx.listener(move |this, _: &text::Up, _window, cx| {
            navigate_list(this, id, count, Key::Up, up_navigation.as_deref(), cx);
        }))
        .on_action(cx.listener(move |this, _: &text::Down, _window, cx| {
            navigate_list(this, id, count, Key::Down, down_navigation.as_deref(), cx);
        }))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
            if let Some(key @ (Key::PageUp | Key::PageDown)) = keyboard::key(event) {
                cx.stop_propagation();
                navigate_list(this, id, count, key, page_navigation.as_deref(), cx);
            }
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
    if !ctx.view.control_state().is_palette_open(id) {
        return None;
    }
    let text = query.read(ctx.cx).text();
    let matches = fuzzy_filter(&text, commands);
    let highlighted = ctx
        .view
        .control_state()
        .list_highlight(id, text.as_str())
        .min(matches.len().saturating_sub(1));
    let on_activate: Activate<V> = Rc::new(move |view: &mut V, picked, window, cx| {
        view.control_state_mut().close_palette(window, cx);
        on_activate(view, picked, window, cx);
    });
    let rows = command_list(id, &matches, highlighted, ctx.reborrow(), {
        let on_activate = on_activate.clone();
        move |view, id, window, cx| on_activate(view, id, window, cx)
    });
    let ids = matches.iter().map(|command| command.id.clone()).collect();
    Some(palette_with_results(
        id,
        query,
        vec![rows.into_any_element()],
        ids,
        None,
        "No matching commands",
        ctx,
        window,
        on_activate,
    ))
}

/// An overlay for results the host already filtered and ranked. It retains
/// the command palette's focus, keyboard and dismissal behavior, while its
/// rows show optional details, leading visuals and source sections.
#[allow(clippy::too_many_arguments)]
pub fn ranked_command_palette<V: ControlHost>(
    id: ComboId,
    query: &Entity<TextInput>,
    results: &[SearchResult],
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Option<impl IntoElement> {
    if !ctx.view.control_state().is_palette_open(id) {
        return None;
    }
    let text = query.read(ctx.cx).text();
    let ids: Vec<SharedString> = results.iter().map(|result| result.id.clone()).collect();
    let (highlighted, changed) = ctx
        .view
        .control_state()
        .list_highlight_with_change(id, (text.as_str(), ids.as_slice()));
    let highlighted = highlighted.min(results.len().saturating_sub(1));
    if changed {
        let scroll = ctx.view.control_state().scroll((id, "list"));
        scroll.set_offset(Default::default());
    }
    let on_activate: Activate<V> = Rc::new(move |view: &mut V, picked, window, cx| {
        view.control_state_mut().close_palette(window, cx);
        on_activate(view, picked, window, cx);
    });
    let rows = ranked_result_rows(
        id,
        &text,
        results,
        highlighted,
        ctx.reborrow(),
        on_activate.clone(),
    );
    Some(palette_with_results(
        id,
        query,
        rows.elements,
        ids,
        Some((rows.child_indices, rows.positions)),
        "No results",
        ctx,
        window,
        on_activate,
    ))
}

/// The palette shell and its keyboard contract are shared by fuzzy commands
/// and host-ranked results, so they cannot drift on Escape, scrim or focus.
#[allow(clippy::too_many_arguments)]
fn palette_with_results<V: ControlHost>(
    id: ComboId,
    query: &Entity<TextInput>,
    rows: Vec<AnyElement>,
    ids: Vec<SharedString>,
    navigation: Option<(Vec<usize>, Vec<f32>)>,
    empty_message: &'static str,
    ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: Activate<V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let state = view.control_state();
    let opened_at = state
        .palette_overlay
        .as_ref()
        .filter(|open| open.id == id)
        .expect("palette was open when its rows were built")
        .opened_at;
    let dark = palette.is_dark;
    let reveal = if cx.reduce_motion() {
        1.0
    } else {
        ease_out_cubic(progress(opened_at, state.scaled(COMBO_REVEAL)))
    };
    let list_scroll = state.scroll((id, "list"));
    let navigation = navigation.map(|(child_indices, positions)| {
        Rc::new(ResultNavigation {
            scroll: list_scroll.clone(),
            child_indices,
            positions,
        })
    });
    let fill = if dark {
        palette.soft_fill
    } else {
        palette.field_surface
    };
    let empty = ids.is_empty();
    let panel = div()
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
                        .children(rows)
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
                                    .child(empty_message),
                            )
                        }),
                )
                .children(crate::scroll::scroll_fades(
                    state,
                    id,
                    &list_scroll,
                    crate::scroll::ScrollAxis::Vertical,
                    fill,
                    fill,
                )),
        );
    let panel = list_keys(panel, id, ids, navigation, cx, on_activate);
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

    #[test]
    fn emphasis_prefers_a_whole_match_and_merges_overlapping_words() {
        assert_eq!(matched_ranges("Design system", "sign des"), vec![0..6]);
        assert_eq!(matched_ranges("Open File", "opf"), vec![0..2, 5..6]);
    }

    #[test]
    fn emphasis_uses_valid_unicode_byte_boundaries() {
        assert_eq!(matched_ranges("Café résumé", "fé su"), vec![2..5, 9..11]);
        assert!(matched_ranges("Café", "").is_empty());
    }

    #[test]
    fn section_headings_do_not_change_result_navigation_indices() {
        let results = [
            SearchResult::new("a", "First")
                .section("Tabs")
                .detail("Detail"),
            SearchResult::new("b", "Second").section("Tabs"),
            SearchResult::new("c", "Third"),
            SearchResult::new("d", "Fourth")
                .section("Bookmarks")
                .detail("Detail"),
        ];
        let layout = ranked_row_layout(&results);
        assert_eq!(
            layout.iter().map(|row| row.child_index).collect::<Vec<_>>(),
            [1, 2, 3, 5]
        );
        assert_eq!(
            layout.iter().map(|row| row.top).collect::<Vec<_>>(),
            [21.0, 66.0, 99.0, 153.0]
        );
    }

    #[test]
    fn page_navigation_moves_by_viewport_and_stops_at_edges() {
        let tops = [21.0, 66.0, 99.0, 153.0, 198.0, 243.0];
        assert_eq!(page_target(Key::PageDown, 0, &tops, 100.0), Some(3));
        assert_eq!(page_target(Key::PageUp, 3, &tops, 100.0), Some(0));
        assert_eq!(page_target(Key::PageDown, 3, &tops, 100.0), Some(5));
        assert_eq!(page_target(Key::PageUp, 0, &tops, 100.0), Some(0));
        assert_eq!(page_target(Key::PageDown, 0, &[], 100.0), None);
    }

    #[test]
    fn highlighted_result_clears_scroll_edge_fades() {
        assert_eq!(revealed_offset(0.0, 300.0, 0.0..100.0, 90.0..110.0), -35.0);
        assert_eq!(
            revealed_offset(-130.0, 300.0, 0.0..100.0, 120.0..140.0),
            -95.0
        );
        assert_eq!(revealed_offset(0.0, 300.0, 0.0..100.0, 0.0..20.0), 0.0);
        assert_eq!(
            revealed_offset(-295.0, 300.0, 0.0..100.0, 380.0..400.0),
            -300.0
        );
    }

    #[test]
    fn long_inline_results_page_to_a_row_inside_their_own_viewport() {
        let results: Vec<_> = (0..14)
            .map(|index| {
                SearchResult::new(format!("result-{index}"), format!("Result {index}"))
                    .section(if index < 7 { "Tabs" } else { "Files" })
                    .detail("A second line")
            })
            .collect();
        let layout = ranked_row_layout(&results);
        let positions: Vec<_> = layout.iter().map(|row| row.top).collect();
        let last = layout.last().expect("results have a last row");
        let max_offset = (last.top + last.height - INLINE_RESULT_MAX_HEIGHT).max(0.0);
        let target = page_target(Key::PageDown, 0, &positions, INLINE_RESULT_MAX_HEIGHT)
            .expect("page down reaches a result");
        assert!(positions[target] >= INLINE_RESULT_MAX_HEIGHT);
        let row = &layout[target];
        let offset = revealed_offset(
            0.0,
            max_offset,
            0.0..INLINE_RESULT_MAX_HEIGHT,
            row.top..row.top + row.height,
        );
        assert!(offset < 0.0);
        assert!(
            row.top + row.height + offset <= INLINE_RESULT_MAX_HEIGHT - crate::scroll::SCROLL_FADE
        );
    }
}
