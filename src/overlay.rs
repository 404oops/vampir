//! Things that float above the view: tooltips and a command palette, and
//! the search field with results that the palette is built from.

use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, AnyView, App, Context, Div, ElementId, Entity, FontWeight, InteractiveElement,
    KeyDownEvent, MouseButton, MouseDownEvent, Point, Render, Rgba, ScrollHandle, SharedString,
    StyledText, Window, deferred, div, point, prelude::*, px,
};

use crate::controls::{WidgetContext, search_field};
use crate::easing::{ease_out_cubic, progress};
use crate::keyboard::{self, Dismiss, Key, Orientation};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{
    COMBO_REVEAL, ComboId, ControlHost, ControlState, ListChange, MOVE, SWITCH_SLIDE, Tag,
};
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
    /// Comes back to the activation callback. Unique within one list: it
    /// names the row's element and the record its motion is kept under.
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

/// One result from a host-ranked search. Its position is already its rank:
/// [`ranked_search_list`] and [`ranked_command_palette`] never filter or
/// sort it, and [`rank_results`] is there for a host that wants the
/// toolkit's fuzzy ranking rather than its own.
///
/// The host may replace the slice at any time, as each source finishes
/// loading. Ids must be unique within one list and stable across updates.
/// Until the person moves the highlight it stays on the first row, so a
/// better match arriving takes it. Once they have moved it, it stays on the
/// result they moved it to, found by its id, so a source that lands after
/// they have arrowed down never leaves Enter on whatever now fills that
/// row. A new query puts it back on the first row either way.
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub id: SharedString,
    pub label: SharedString,
    /// A second, quieter line under the label.
    pub detail: Option<SharedString>,
    /// Shown once before each run of results from the same source.
    pub section: Option<SharedString>,
    pub shortcut: Option<SharedString>,
    leading: Option<Leading>,
    /// A [`Command`]'s group, which a [`command_list`] shows before the
    /// label rather than as a heading: commands are ranked across groups,
    /// so one group's commands seldom arrive in a run.
    group: Option<SharedString>,
}

/// The host's drawing for a result's leading slot, in a type of its own so
/// that [`SearchResult`] can still be `Debug`.
#[derive(Clone)]
struct Leading(Rc<dyn Fn(Palette) -> AnyElement>);

impl std::fmt::Debug for Leading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Leading(..)")
    }
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
            group: None,
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
        self.leading = Some(Leading(Rc::new(move |palette| {
            render(palette).into_any_element()
        })));
        self
    }
}

/// A command as a result row, so commands and results share one drawing.
fn command_result(command: &Command) -> SearchResult {
    SearchResult {
        id: command.id.clone(),
        label: command.label.clone(),
        detail: None,
        section: None,
        shortcut: command.shortcut.clone(),
        leading: None,
        group: command.group.clone(),
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

/// Results matching a query, best first within each section, for a host
/// that would rather rank with [`fuzzy_score`] than with its own measure
/// before handing them to [`ranked_search_list`] or
/// [`ranked_command_palette`].
///
/// Each section appears once, its results together under one heading, and
/// sections are ordered by their best result: ranking across every source
/// at once would scatter them into a heading per row. The label, the detail
/// and the section are all searched, so "book" finds everything under
/// Bookmarks. Ties keep the order the sources were assembled in, and an
/// empty query keeps source order within each section and puts sections in
/// their first-seen order.
pub fn rank_results(query: &str, results: &[SearchResult]) -> Vec<SearchResult> {
    // Scored on the label alone: it is what the person reads and
    // abbreviates, and the score's length penalty would otherwise let a
    // long detail line push a result down. A result that matches only
    // through its detail or section still qualifies, after every result
    // whose label matched, in the order it came.
    let rank = |result: &SearchResult| -> Option<Option<i32>> {
        if let Some(score) = fuzzy_score(query, &result.label) {
            return Some(Some(score));
        }
        let mut text = result.label.to_string();
        for part in [&result.detail, &result.section].into_iter().flatten() {
            text.push(' ');
            text.push_str(part);
        }
        fuzzy_score(query, &text).map(|_| None)
    };
    type Run<'a> = Vec<(Option<i32>, &'a SearchResult)>;
    let mut sections: Vec<(Option<&str>, Run)> = Vec::new();
    for result in results {
        let Some(rank) = rank(result) else {
            continue;
        };
        let section = result.section.as_deref();
        match sections.iter_mut().find(|(name, _)| *name == section) {
            Some((_, run)) => run.push((rank, result)),
            None => sections.push((section, vec![(rank, result)])),
        }
    }
    // Stable sorts, so ties keep the order the sources were assembled in.
    for (_, run) in &mut sections {
        run.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    }
    sections.sort_by_key(|(_, run)| std::cmp::Reverse(run[0].0));
    sections
        .into_iter()
        .flat_map(|(_, run)| run)
        .map(|(_, result)| result.clone())
        .collect()
}

/// Which line of a row is being emphasised.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Label,
    Detail,
}

/// Byte ranges to emphasise in displayed text, for each word of the query.
///
/// In a label, a contiguous run of the word anywhere reads best, and
/// failing one the letters of a fuzzy abbreviation are marked wherever they
/// fall: the label is what the person was abbreviating. A detail line is
/// not, so there only a contiguous run that starts a word counts — "in"
/// picks out "in" but not the middle of "morning", and scattered letters
/// that happen to come in the right order are not a match at all.
fn matched_ranges(text: &str, query: &str, line: Line) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let mut ranges = Vec::new();
    for word in query.split_whitespace() {
        let needle: Vec<char> = word.chars().collect();
        let starts_word = |start: usize| start == 0 || !chars[start - 1].1.is_alphanumeric();
        let whole = (0..=chars.len().saturating_sub(needle.len()))
            .find(|&start| {
                start + needle.len() <= chars.len()
                    && (line == Line::Label || starts_word(start))
                    && needle
                        .iter()
                        .enumerate()
                        .all(|(offset, &ch)| same(chars[start + offset].1, ch))
            })
            .map(|start| (start..start + needle.len()).collect::<Vec<_>>());
        let positions = whole.or_else(|| {
            if line == Line::Detail {
                return None;
            }
            let mut found = Vec::new();
            let mut from = 0;
            for &ch in &needle {
                let index = (from..chars.len()).find(|&i| same(chars[i].1, ch))?;
                found.push(index);
                from = index + 1;
            }
            Some(found)
        });
        for index in positions.into_iter().flatten() {
            let start = chars[index].0;
            let end = chars.get(index + 1).map_or(text.len(), |next| next.0);
            ranges.push(start..end);
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

fn emphasised(text: &SharedString, query: &str, line: Line) -> StyledText {
    StyledText::new(text.to_string()).with_highlights(
        matched_ranges(text, query, line)
            .into_iter()
            .map(|range| (range, FontWeight::SEMIBOLD.into())),
    )
}

/// Height of one row in a [`command_list`], and of any result row without
/// a detail line.
pub const COMMAND_ROW_HEIGHT: f32 = 30.0;
/// Height of a result row with a detail line under its label.
const DETAIL_ROW_HEIGHT: f32 = 44.0;
const SECTION_HEIGHT: f32 = 20.0;
const ROW_GAP: f32 = 1.0;
const ROW_INSET: f32 = 9.0;
const LEADING_SIZE: f32 = 20.0;
const ROW_SPACING: f32 = 8.0;
/// The tallest an inline result list grows before it scrolls.
const INLINE_RESULT_MAX_HEIGHT: f32 = 300.0;
/// The same for the list in a palette, which has the window to itself.
const PALETTE_RESULT_MAX_HEIGHT: f32 = 340.0;

struct RowPlacement {
    /// Whether a section heading goes above this row.
    heading: bool,
    top: f32,
    height: f32,
}

/// Where each result sits in its list. Worked out from the row metrics
/// rather than read back from the last layout, so a highlight that moved
/// with its result can be scrolled to in the same frame the results
/// changed, before anything has been laid out.
fn row_layout(results: &[SearchResult]) -> Vec<RowPlacement> {
    let mut previous: Option<&str> = None;
    let mut top = 0.0;
    results
        .iter()
        .map(|result| {
            let section = result.section.as_deref();
            let heading = section.is_some() && section != previous;
            previous = section;
            if heading {
                top += SECTION_HEIGHT + ROW_GAP;
            }
            let height = if result.detail.is_some() {
                DETAIL_ROW_HEIGHT
            } else {
                COMMAND_ROW_HEIGHT
            };
            let placement = RowPlacement {
                heading,
                top,
                height,
            };
            top += height + ROW_GAP;
            placement
        })
        .collect()
}

/// How far a row or heading has arrived. A list that has just appeared
/// arrives whole, with whatever brought it. Once it is up, rows the query
/// lets through fade in at their place and rows that stay slide to their
/// new one, so typing reads as the list being sifted rather than replaced.
fn arrival(state: &ControlState, key: Tag, fresh: bool) -> f32 {
    if fresh {
        state.tween(key, 1.0, SWITCH_SLIDE)
    } else {
        state.tween_from(key, 0.0, 1.0, SWITCH_SLIDE)
    }
}

/// The rows of a result list in the order given, and where each result
/// sits. Section names are quiet headings rather than rows; only results
/// take a keyboard step. `emphasis` is the text to pick out in each row.
fn result_rows<V: ControlHost>(
    id: &'static str,
    emphasis: &str,
    results: &[SearchResult],
    highlighted: usize,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_activate: Activate<V>,
) -> (Vec<AnyElement>, Vec<Range<f32>>) {
    let WidgetContext { palette, view, cx } = ctx;
    let state = view.control_state();
    let fresh = !state.present(id);
    let has_leading = results.iter().any(|result| result.leading.is_some());
    let mut elements: Vec<AnyElement> = Vec::with_capacity(results.len());
    let mut extents = Vec::with_capacity(results.len());
    let mut headed: Vec<&str> = Vec::new();
    for (index, (result, placement)) in results.iter().zip(row_layout(results)).enumerate() {
        if placement.heading
            && let Some(section) = &result.section
        {
            // Keyed by the section, and by which of its runs this is should
            // a host split one, rather than by the result under it: whichever
            // result leads the run, it is the same heading, and it fades in
            // once and slides with the rows.
            let run = headed
                .iter()
                .filter(|&&name| name == section.as_ref())
                .count();
            headed.push(section);
            let key = |part: &'static str| Tag::new((id, part, section, run));
            let shown = arrival(state, key("section-shown"), fresh);
            let top = placement.top - SECTION_HEIGHT - ROW_GAP;
            let offset = state.tween(key("section-place"), top, MOVE) - top;
            elements.push(section_heading(
                section,
                has_leading,
                shown,
                offset,
                palette,
            ));
        }
        let cx: &mut Context<V> = &mut *cx;
        let look = RowLook {
            active: index == highlighted,
            has_leading,
            fresh,
            palette,
        };
        elements.push(result_row(
            id,
            result,
            emphasis,
            &placement,
            look,
            state,
            cx,
            on_activate.clone(),
        ));
        extents.push(placement.top..placement.top + placement.height);
    }
    (elements, extents)
}

fn section_heading(
    section: &SharedString,
    has_leading: bool,
    shown: f32,
    offset: f32,
    palette: Palette,
) -> AnyElement {
    // Over the labels rather than the icons, so a heading reads as the
    // title of the run of results under it.
    let inset = if has_leading {
        ROW_INSET + LEADING_SIZE + ROW_SPACING
    } else {
        ROW_INSET
    };
    div()
        .relative()
        .top(px(offset))
        .h(px(SECTION_HEIGHT))
        .flex_none()
        .px(px(inset))
        .flex()
        .items_center()
        .opacity(shown)
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(palette.text_secondary)
        .child(section.clone())
        .into_any_element()
}

#[derive(Clone, Copy)]
struct RowLook {
    active: bool,
    has_leading: bool,
    fresh: bool,
    palette: Palette,
}

/// One row of a result list or a command list: the same chrome for both,
/// so a command palette and a results palette cannot drift apart.
#[allow(clippy::too_many_arguments)]
fn result_row<V: ControlHost>(
    id: &'static str,
    result: &SearchResult,
    emphasis: &str,
    placement: &RowPlacement,
    look: RowLook,
    state: &ControlState,
    cx: &mut Context<V>,
    on_activate: Activate<V>,
) -> AnyElement {
    let RowLook {
        active,
        has_leading,
        fresh,
        palette,
    } = look;
    let key = |part: &'static str| Tag::new((id, part, &result.id));
    // Keyed by the result rather than the row, so the highlight and the
    // fade belong to the result as it moves.
    let on = state.blend(key("lit"), active, SWITCH_SLIDE);
    // Secondary ink is for unselected rows; all selected text needs
    // the foreground paired with the accent surface underneath it.
    let secondary = crate::color::lerp(palette.text_secondary, palette.soft_label, on);
    let shown = arrival(state, key("shown"), fresh);
    let offset = state.tween(key("place"), placement.top, MOVE) - placement.top;
    let picked = result.id.clone();
    div()
        .id(ElementId::Name(format!("{id}-result-{}", result.id).into()))
        .relative()
        .top(px(offset))
        .opacity(shown)
        .h(px(placement.height))
        .flex_none()
        .w_full()
        .px(px(ROW_INSET))
        .flex()
        .items_center()
        .gap(px(ROW_SPACING))
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
            on_activate(this, picked.clone(), window, cx);
            cx.notify();
        }))
        .when(has_leading, |el| {
            el.child(
                div()
                    .size(px(LEADING_SIZE))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(result.leading.as_ref().map(|leading| (leading.0)(palette))),
            )
        })
        .children(
            result
                .group
                .clone()
                .map(|group| div().flex_none().text_color(secondary).child(group)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .truncate()
                        .child(emphasised(&result.label, emphasis, Line::Label)),
                )
                .children(result.detail.as_ref().map(|detail| {
                    div()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(secondary)
                        .child(emphasised(detail, emphasis, Line::Detail))
                })),
        )
        .children(result.shortcut.clone().map(|shortcut| {
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(secondary)
                .child(shortcut)
        }))
        .into_any_element()
}

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
    // A concrete `Div` rather than `impl IntoElement`: in the 2024 edition an
    // opaque return captures every input lifetime, which would keep `cx`
    // borrowed for as long as the list lived and stop the caller building
    // anything else with it.
    let results: Vec<SearchResult> = matches.iter().map(command_result).collect();
    let (rows, _) = result_rows(id, "", &results, highlighted, ctx, Rc::new(on_activate));
    // Clipped, so a row sliding up from where it was emerges from the list's
    // edge instead of crossing whatever sits below it.
    div()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
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
    let on_activate: Activate<V> = Rc::new(on_activate);
    let text = query.read(ctx.cx).text();
    let matches = fuzzy_filter(&text, items);
    let ids: Rc<[SharedString]> = matches.iter().map(|command| command.id.clone()).collect();
    let (highlighted, _) = ctx.view.control_state().follow_list(id, &text, &ids);
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
    let list = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(search_field(id, query, palette, window, cx))
        .child(rows);
    list_keys(list, id, ids, None, cx, on_activate)
}

/// A search field over results the host has already filtered and ranked,
/// in a list that scrolls under the field once it is taller than a few
/// rows. The keys work as in [`search_list`], except that Page Up and Page
/// Down move a viewport at a time; section headings take no keyboard step,
/// and the highlighted row is kept in view.
///
/// The host may replace `results` at any time. A new query puts the
/// highlight back on the first result and the list back at its top. Results
/// that change under the same query leave an untouched highlight on the
/// first row, and keep one the person has moved on the result they moved it
/// to, by id, scrolling to keep it in view — see [`SearchResult`].
///
/// `start` and `end` are the colours of the surface under the list's top
/// and bottom edges, which its edge fades land on, as for
/// [`scroll_fades`](crate::scroll_fades); the list's viewport is
/// [`ControlState::scroll`](crate::ControlState::scroll)`((id, "list"))`,
/// whose bounds say where those edges are. `id` identifies the list in
/// [`ControlState`](crate::ControlState), as for [`search_list`].
#[allow(clippy::too_many_arguments)]
pub fn ranked_search_list<V: ControlHost>(
    id: &'static str,
    query: &Entity<TextInput>,
    results: &[SearchResult],
    start: Rgba,
    end: Rgba,
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Div {
    let palette = ctx.palette;
    let text = query.read(ctx.cx).text();
    let on_activate: Activate<V> = Rc::new(on_activate);
    let list = result_list(
        id,
        &text,
        &text,
        results,
        INLINE_RESULT_MAX_HEIGHT,
        ctx.reborrow(),
        on_activate.clone(),
    );
    let state = ctx.view.control_state();
    let cx: &mut Context<V> = &mut *ctx.cx;
    let element = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(search_field(id, query, palette, window, cx))
        .child(result_viewport(
            id,
            list.elements,
            &list.navigation,
            "No results",
            (start, end),
            palette,
            state,
        ));
    list_keys(
        element,
        id,
        list.ids,
        Some(list.navigation),
        cx,
        on_activate,
    )
}

/// A scrolling result list's rows, with what its keys need to move through
/// them.
struct ResultList {
    elements: Vec<AnyElement>,
    ids: Rc<[SharedString]>,
    navigation: Rc<ResultNavigation>,
}

/// Draws a scrolling result list, keeping the keyboard's row on its result
/// as the results change and the list's scroll in step with the row.
#[allow(clippy::too_many_arguments)]
fn result_list<V: ControlHost>(
    id: &'static str,
    query: &str,
    emphasis: &str,
    results: &[SearchResult],
    max_height: f32,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_activate: Activate<V>,
) -> ResultList {
    let ids: Rc<[SharedString]> = results.iter().map(|result| result.id.clone()).collect();
    let state = ctx.view.control_state();
    let (highlighted, change) = state.follow_list(id, query, &ids);
    let scroll = state.scroll((id, "list"));
    let (elements, rows) = result_rows(id, emphasis, results, highlighted, ctx, on_activate);
    let navigation = Rc::new(ResultNavigation {
        scroll,
        rows,
        max_height,
    });
    match change {
        ListChange::Reset => navigation.scroll.set_offset(Point::default()),
        ListChange::Moved => navigation.reveal(highlighted),
        ListChange::Same => {}
    }
    ResultList {
        elements,
        ids,
        navigation,
    }
}

/// A result list's rows in a viewport that scrolls under the query field,
/// which stays where it is.
fn result_viewport(
    id: &'static str,
    rows: Vec<AnyElement>,
    navigation: &ResultNavigation,
    empty_message: &'static str,
    (start, end): (Rgba, Rgba),
    palette: Palette,
    state: &ControlState,
) -> Div {
    let empty = rows.is_empty();
    div()
        .relative()
        .child(
            div()
                .id(ElementId::Name(format!("{id}-list").into()))
                .max_h(px(navigation.max_height))
                .flex()
                .flex_col()
                .gap(px(ROW_GAP))
                .overflow_y_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&navigation.scroll)
                .children(rows)
                .when(empty, |el| {
                    el.child(
                        div()
                            .h(px(COMMAND_ROW_HEIGHT))
                            .px(px(ROW_INSET))
                            .flex()
                            .items_center()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(palette.text_secondary)
                            .child(empty_message),
                    )
                }),
        )
        // A long list fades at the edge that has more past it, rather than
        // slicing a row in half.
        .children(crate::scroll::scroll_fades(
            state,
            id,
            &navigation.scroll,
            crate::scroll::ScrollAxis::Vertical,
            start,
            end,
        ))
}

/// Where a scrolling list's results are, for the keys that move through it.
struct ResultNavigation {
    scroll: ScrollHandle,
    /// Each result's extent, top to bottom, in the list's own coordinates.
    rows: Vec<Range<f32>>,
    max_height: f32,
}

impl ResultNavigation {
    fn content_height(&self) -> f32 {
        self.rows.last().map_or(0.0, |row| row.end)
    }

    fn viewport_height(&self) -> f32 {
        self.content_height().min(self.max_height)
    }

    /// Scrolls just far enough to bring result `index` clear of the edge
    /// fades.
    fn reveal(&self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let viewport = self.viewport_height();
        let offset = self.scroll.offset();
        let next = revealed_offset(
            f32::from(offset.y),
            self.content_height() - viewport,
            0.0..viewport,
            row.clone(),
        );
        self.scroll.set_offset(point(offset.x, px(next)));
    }
}

/// The result a page away from `current`: the first whose top is a whole
/// viewport past it, or the end of the list.
fn page_target(key: Key, current: usize, rows: &[Range<f32>], viewport: f32) -> Option<usize> {
    let last = rows.len().checked_sub(1)?;
    let current = current.min(last);
    let page = viewport.max(1.0);
    match key {
        Key::PageDown => {
            let threshold = rows[current].start + page;
            Some(
                ((current + 1)..=last)
                    .find(|&index| rows[index].start >= threshold)
                    .unwrap_or(last),
            )
        }
        Key::PageUp => {
            let threshold = rows[current].start - page;
            Some(
                (0..current)
                    .rev()
                    .find(|&index| rows[index].start <= threshold)
                    .unwrap_or(0),
            )
        }
        _ => None,
    }
}

/// The scroll offset that shows `item` inside `viewport`, short of the
/// edge fades, moving as little as it can.
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

fn navigate_list<V: ControlHost>(
    this: &mut V,
    id: &'static str,
    ids: &[SharedString],
    key: Key,
    navigation: Option<&ResultNavigation>,
    cx: &mut Context<V>,
) {
    let count = ids.len();
    let state = this.control_state();
    let here = state.list_position(id).min(count.saturating_sub(1));
    let moved = match (key, navigation) {
        (Key::PageUp | Key::PageDown, Some(navigation)) => {
            page_target(key, here, &navigation.rows, navigation.viewport_height())
        }
        // A list that does not scroll has no page to measure, so Page moves
        // ten, as it does on a slider.
        (Key::PageUp, None) => count.checked_sub(1).map(|_| here.saturating_sub(10)),
        (Key::PageDown, None) => count.checked_sub(1).map(|last| (here + 10).min(last)),
        _ => keyboard::step(key, Orientation::Vertical, here, count),
    };
    if let Some(moved) = moved
        && let Some(picked) = ids.get(moved)
    {
        state.highlight_list_item(id, moved, picked);
        if let Some(navigation) = navigation {
            navigation.reveal(moved);
        }
        cx.notify();
    }
}

/// Puts a filtering list's keys on the element that holds its query field:
/// Up and Down move the highlight through `ids`, Page Up and Page Down move
/// it a page, Enter picks the highlighted one.
fn list_keys<V: ControlHost, E: InteractiveElement>(
    element: E,
    id: &'static str,
    ids: Rc<[SharedString]>,
    navigation: Option<Rc<ResultNavigation>>,
    cx: &mut Context<V>,
    on_activate: Activate<V>,
) -> E {
    let step = {
        let ids = ids.clone();
        Rc::new(move |this: &mut V, key: Key, cx: &mut Context<V>| {
            navigate_list(this, id, &ids, key, navigation.as_deref(), cx);
        })
    };
    let step_down = step.clone();
    let step_page = step.clone();
    element
        .on_action(cx.listener(move |this, _: &text::Up, _window, cx| {
            step(this, Key::Up, cx);
        }))
        .on_action(cx.listener(move |this, _: &text::Down, _window, cx| {
            step_down(this, Key::Down, cx);
        }))
        // The field has no actions for Page Up and Page Down, so they
        // arrive as keys.
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
            if let Some(key @ (Key::PageUp | Key::PageDown)) = keyboard::key(event) {
                cx.stop_propagation();
                step_page(this, key, cx);
            }
        }))
        .on_action(cx.listener(move |this, _: &text::Enter, window, cx| {
            let here = this
                .control_state()
                .list_position(id)
                .min(ids.len().saturating_sub(1));
            if let Some(picked) = ids.get(here) {
                on_activate(this, picked.clone(), window, cx);
                cx.notify();
            }
        }))
}

/// When palette `id` opened, if it is the one open.
fn palette_opened_at(state: &ControlState, id: ComboId) -> Option<Instant> {
    state
        .palette_overlay
        .as_ref()
        .filter(|open| open.id == id)
        .map(|open| open.opened_at)
}

/// The host's callback, run after the palette closes, which hands the
/// keyboard back where it was, so a command that opens something starts
/// from the right place.
fn closing_palette<V: ControlHost>(
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Activate<V> {
    Rc::new(move |view: &mut V, picked, window, cx| {
        view.control_state_mut().close_palette(window, cx);
        on_activate(view, picked, window, cx);
    })
}

/// Centred overlay with a filter field and a list of matching commands.
///
/// Open it with [`ControlState::open_palette`](crate::ControlState::open_palette)
/// — or [`toggle_palette`](crate::ControlState::toggle_palette), from
/// whatever shortcut the host gives it; while `id` is not open this returns
/// nothing. The host owns the query input and the commands. The palette
/// filters them with [`fuzzy_filter`], moves the highlight with Up, Down,
/// Page Up and Page Down, keeping it in view, runs the highlighted command
/// on Enter or a clicked one, and closes on Escape or a press on the scrim.
/// It closes before a command runs, handing the keyboard back where it was,
/// so a command that opens something starts from the right place.
#[allow(clippy::too_many_arguments)]
pub fn command_palette<V: ControlHost>(
    id: ComboId,
    query: &Entity<TextInput>,
    commands: &[Command],
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Option<impl IntoElement> {
    let opened_at = palette_opened_at(ctx.view.control_state(), id)?;
    let text = query.read(ctx.cx).text();
    let matches: Vec<SearchResult> = fuzzy_filter(&text, commands)
        .iter()
        .map(command_result)
        .collect();
    let on_activate = closing_palette(on_activate);
    let list = result_list(
        id,
        &text,
        "",
        &matches,
        PALETTE_RESULT_MAX_HEIGHT,
        ctx.reborrow(),
        on_activate.clone(),
    );
    Some(palette_with_results(
        id,
        opened_at,
        query,
        list,
        "No matching commands",
        ctx,
        window,
        on_activate,
    ))
}

/// An overlay for results the host has already filtered and ranked: a
/// [`command_palette`] in every way that concerns opening, the keyboard and
/// closing, whose rows are [`SearchResult`]s, kept on their result as the
/// results change, exactly as in [`ranked_search_list`].
#[allow(clippy::too_many_arguments)]
pub fn ranked_command_palette<V: ControlHost>(
    id: ComboId,
    query: &Entity<TextInput>,
    results: &[SearchResult],
    mut ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
) -> Option<impl IntoElement> {
    let opened_at = palette_opened_at(ctx.view.control_state(), id)?;
    let text = query.read(ctx.cx).text();
    let on_activate = closing_palette(on_activate);
    let list = result_list(
        id,
        &text,
        &text,
        results,
        PALETTE_RESULT_MAX_HEIGHT,
        ctx.reborrow(),
        on_activate.clone(),
    );
    Some(palette_with_results(
        id,
        opened_at,
        query,
        list,
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
    opened_at: Instant,
    query: &Entity<TextInput>,
    list: ResultList,
    empty_message: &'static str,
    ctx: WidgetContext<'_, '_, '_, V>,
    window: &Window,
    on_activate: Activate<V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let state = view.control_state();
    let dark = palette.is_dark;
    // The panel arrives the way a menu does: the same reveal, a short drift
    // down into place.
    let reveal = ease_out_cubic(progress(opened_at, state.scaled(COMBO_REVEAL)));
    let fill = if dark {
        palette.soft_fill
    } else {
        palette.field_surface
    };
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
        .child(result_viewport(
            id,
            list.elements,
            &list.navigation,
            empty_message,
            (fill, fill),
            palette,
            state,
        ));
    let panel = list_keys(panel, id, list.ids, Some(list.navigation), cx, on_activate);
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
        assert_eq!(
            matched_ranges("Design system", "sign des", Line::Label),
            vec![0..6]
        );
        assert_eq!(
            matched_ranges("Open File", "opf", Line::Label),
            vec![0..2, 5..6]
        );
    }

    #[test]
    fn emphasis_uses_valid_unicode_byte_boundaries() {
        assert_eq!(
            matched_ranges("Café résumé", "fé su", Line::Label),
            vec![2..5, 9..11]
        );
        assert!(matched_ranges("Café", "", Line::Label).is_empty());
    }

    /// A detail line is not what the person abbreviated, so scattered
    /// letters there are coincidence, and so is a run inside a word.
    #[test]
    fn a_detail_line_emphasises_only_runs_that_start_a_word() {
        let detail = "Project board · 2 minutes ago";
        assert!(matched_ranges(detail, "de", Line::Detail).is_empty());
        assert_eq!(matched_ranges(detail, "boa", Line::Detail), vec![8..11]);
        assert!(matched_ranges("Visited this morning", "in", Line::Detail).is_empty());
        assert_eq!(
            matched_ranges("Visited this morning", "mor", Line::Detail),
            vec![13..16]
        );
        assert_eq!(
            matched_ranges("Design review", "de", Line::Label),
            vec![0..2]
        );
        assert_eq!(matched_ranges("Morning", "in", Line::Label), vec![4..6]);
    }

    /// The gallery's sources, in the order it assembles them.
    fn sources() -> Vec<SearchResult> {
        [
            (
                "tab-design",
                "Design review",
                "Project board · 2 minutes ago",
                "Open tabs",
            ),
            (
                "tab-release",
                "Release checklist",
                "Draft · Current window",
                "Open tabs",
            ),
            (
                "tab-insights",
                "Traffic insights",
                "Analytics · Current window",
                "Open tabs",
            ),
            ("tab-issues", "Issue tracker", "42 open tasks", "Open tabs"),
            (
                "bookmark-system",
                "Design system",
                "Components and colour recipes",
                "Bookmarks",
            ),
            (
                "bookmark-keys",
                "Keyboard shortcuts",
                "Reference · Documentation",
                "Bookmarks",
            ),
            (
                "bookmark-api",
                "API reference",
                "Framework documentation",
                "Bookmarks",
            ),
            (
                "bookmark-roadmap",
                "Project roadmap",
                "Milestones and plans",
                "Bookmarks",
            ),
            (
                "file-budget",
                "Quarterly budget",
                "Shared documents · Updated today",
                "Files",
            ),
            (
                "file-summary",
                "Meeting summary",
                "Notes · Updated yesterday",
                "Files",
            ),
            (
                "history-notes",
                "Design notes",
                "Visited this morning",
                "History",
            ),
            (
                "history-changelog",
                "Release notes",
                "Visited yesterday",
                "History",
            ),
            (
                "action-invite",
                "Invite teammate",
                "Share this workspace",
                "Actions",
            ),
            (
                "action-export",
                "Export report",
                "Save a local copy",
                "Actions",
            ),
        ]
        .into_iter()
        .map(|(id, label, detail, section)| {
            SearchResult::new(id, label).detail(detail).section(section)
        })
        .collect()
    }

    #[test]
    fn ranking_keeps_each_section_together() {
        let sources = sources();
        for query in ["", "de", "re", "design", "no", "book"] {
            let ranked = rank_results(query, &sources);
            let mut seen: Vec<&str> = Vec::new();
            for result in &ranked {
                let section = result
                    .section
                    .as_deref()
                    .expect("every source has a section");
                if seen.last() != Some(&section) {
                    assert!(
                        !seen.contains(&section),
                        "{section} heads two runs for {query:?}"
                    );
                    seen.push(section);
                }
            }
        }
        let ids = |query: &str| -> Vec<SharedString> {
            rank_results(query, &sources)
                .into_iter()
                .map(|result| result.id)
                .collect()
        };
        let all: Vec<SharedString> = sources.iter().map(|result| result.id.clone()).collect();
        assert_eq!(ids(""), all);
        // Equal scores, so the sections keep the order they came in.
        assert_eq!(
            ids("design"),
            ["tab-design", "bookmark-system", "history-notes"]
        );
        // Through the section alone, in source order.
        assert_eq!(
            ids("book"),
            [
                "bookmark-system",
                "bookmark-keys",
                "bookmark-api",
                "bookmark-roadmap"
            ]
        );
        assert!(ids("zzz").is_empty());
    }

    #[test]
    fn ranking_scores_the_label_not_the_length_of_the_detail() {
        let results = [
            SearchResult::new("long", "Design review")
                .detail("A detail line long enough to cost several points of length"),
            SearchResult::new("short", "Design notes").detail("Brief"),
            SearchResult::new("detail-only", "Weekly sync").detail("Design"),
        ];
        let ranked: Vec<SharedString> = rank_results("design", &results)
            .into_iter()
            .map(|result| result.id)
            .collect();
        assert_eq!(ranked, ["long", "short", "detail-only"]);
    }

    #[test]
    fn section_headings_take_room_but_no_keyboard_step() {
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
        let layout = row_layout(&results);
        assert_eq!(
            layout.iter().map(|row| row.heading).collect::<Vec<_>>(),
            [true, false, false, true]
        );
        assert_eq!(
            layout.iter().map(|row| row.top).collect::<Vec<_>>(),
            [21.0, 66.0, 97.0, 149.0]
        );
        assert_eq!(
            layout.iter().map(|row| row.height).collect::<Vec<_>>(),
            [
                DETAIL_ROW_HEIGHT,
                COMMAND_ROW_HEIGHT,
                COMMAND_ROW_HEIGHT,
                DETAIL_ROW_HEIGHT
            ]
        );
    }

    #[test]
    fn page_navigation_moves_by_viewport_and_stops_at_edges() {
        let rows: Vec<Range<f32>> = [21.0, 66.0, 99.0, 153.0, 198.0, 243.0]
            .into_iter()
            .map(|top| top..top + 30.0)
            .collect();
        assert_eq!(page_target(Key::PageDown, 0, &rows, 100.0), Some(3));
        assert_eq!(page_target(Key::PageUp, 3, &rows, 100.0), Some(0));
        assert_eq!(page_target(Key::PageDown, 3, &rows, 100.0), Some(5));
        assert_eq!(page_target(Key::PageUp, 0, &rows, 100.0), Some(0));
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
        let navigation = ResultNavigation {
            scroll: ScrollHandle::new(),
            rows: row_layout(&results)
                .iter()
                .map(|row| row.top..row.top + row.height)
                .collect(),
            max_height: INLINE_RESULT_MAX_HEIGHT,
        };
        let viewport = navigation.viewport_height();
        assert_eq!(viewport, INLINE_RESULT_MAX_HEIGHT);
        let target = page_target(Key::PageDown, 0, &navigation.rows, viewport)
            .expect("page down reaches a result");
        let row = navigation.rows[target].clone();
        assert!(row.start >= viewport);
        let offset = revealed_offset(
            0.0,
            navigation.content_height() - viewport,
            0.0..viewport,
            row.clone(),
        );
        assert!(offset < 0.0);
        assert!(row.end + offset <= viewport - crate::scroll::SCROLL_FADE);
    }

    /// A list shorter than its cap is all viewport, so nothing scrolls and
    /// a page goes to the end.
    #[test]
    fn a_short_list_is_its_own_viewport() {
        let results: Vec<_> = (0..3)
            .map(|index| SearchResult::new(format!("r{index}"), "Result"))
            .collect();
        let navigation = ResultNavigation {
            scroll: ScrollHandle::new(),
            rows: row_layout(&results)
                .iter()
                .map(|row| row.top..row.top + row.height)
                .collect(),
            max_height: PALETTE_RESULT_MAX_HEIGHT,
        };
        assert_eq!(navigation.content_height(), 3.0 * COMMAND_ROW_HEIGHT + 2.0);
        assert_eq!(navigation.viewport_height(), navigation.content_height());
        assert_eq!(
            page_target(
                Key::PageDown,
                0,
                &navigation.rows,
                navigation.viewport_height()
            ),
            Some(2)
        );
    }
}
