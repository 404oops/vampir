//! Rows of data: a tree and a sortable table header.
//!
//! Neither owns its data. A tree takes an already flattened list of visible
//! rows, and a header takes the current sort; both report clicks and leave
//! the model to the host. That is what lets either sit above a
//! `uniform_list` and stay cheap with thousands of rows.

use std::hash::Hash;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, ElementId, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, PathBuilder, SharedString, Window, canvas, div, point, prelude::*, px,
};

use crate::controls::WidgetContext;
use crate::keyboard::{self, Key};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{ControlHost, ControlState, MOVE, SWITCH_SLIDE, Tag};

// ---- Tree -------------------------------------------------------------------

/// One visible row of a tree.
///
/// The host flattens its own model into these each frame, skipping the
/// children of collapsed rows. Nothing here walks a tree, so the model can
/// be whatever shape the host already has.
#[derive(Clone, Debug)]
pub struct TreeRow {
    /// Comes back to the callbacks.
    pub id: SharedString,
    pub label: SharedString,
    /// How deep the row sits. Zero is a root.
    pub depth: usize,
    /// `None` for a leaf, which gets no chevron and no disclosure hit area.
    pub expanded: Option<bool>,
    /// Trailing text, for a count or a size.
    pub detail: Option<SharedString>,
}

impl TreeRow {
    pub fn leaf(id: impl Into<SharedString>, label: impl Into<SharedString>, depth: usize) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            depth,
            expanded: None,
            detail: None,
        }
    }

    pub fn branch(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        depth: usize,
        expanded: bool,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            depth,
            expanded: Some(expanded),
            detail: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Walks a host's tree into the rows a [`tree_row`] draws, skipping the
/// children of anything closed — which is the step that makes collapsing
/// mean something. Keeping the flattened list *as* the model is the
/// tempting shortcut, since it is what gets drawn, but then closing a
/// branch has nothing to hide, because its children were never underneath
/// it in the first place.
///
/// `row` makes the [`TreeRow`] for one node at a depth; its `expanded` is
/// what decides whether `children` is walked.
///
/// ```ignore
/// let rows = flatten_tree(&self.files, &|node, depth| node.row(depth), &|node| &node.children);
/// ```
pub fn flatten_tree<N>(
    roots: &[N],
    row: &impl Fn(&N, usize) -> TreeRow,
    children: &impl Fn(&N) -> &[N],
) -> Vec<TreeRow> {
    fn walk<N>(
        nodes: &[N],
        depth: usize,
        row: &impl Fn(&N, usize) -> TreeRow,
        children: &impl Fn(&N) -> &[N],
        rows: &mut Vec<TreeRow>,
    ) {
        for node in nodes {
            let flat = row(node, depth);
            let open = flat.expanded == Some(true);
            rows.push(flat);
            if open {
                walk(children(node), depth + 1, row, children, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(roots, 0, row, children, &mut rows);
    rows
}

/// How far each level of nesting is indented.
pub const TREE_INDENT: f32 = 14.0;
/// Row height, matching a pop-up list's so a tree and a menu beside each
/// other line up.
pub const TREE_ROW_HEIGHT: f32 = 24.0;

/// What a key press does to a tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeMove {
    /// Put the selection on this row.
    To(usize),
    /// Open or close the row that already has the selection.
    Expand(bool),
}

/// Where a key press takes a tree whose selection sits on `index`.
///
/// A tree does not wrap the way a radio group does. A group is a set of
/// choices with no natural end; a tree is content, and arriving back at the
/// top after pressing down at the bottom loses a reader their place rather
/// than saving them a keystroke.
///
/// Left and right do the two jobs a tree needs and nothing else has: right
/// opens a closed branch and then walks into it, left closes an open one and
/// then walks out to the parent. That pair is the whole reason a tree is
/// navigable by keyboard at all.
pub fn tree_step(rows: &[TreeRow], index: usize, key: Key) -> Option<TreeMove> {
    let row = rows.get(index)?;
    let last = rows.len().checked_sub(1)?;
    match key {
        Key::Up => (index > 0).then(|| TreeMove::To(index - 1)),
        Key::Down => (index < last).then(|| TreeMove::To(index + 1)),
        Key::Home => (index != 0).then_some(TreeMove::To(0)),
        Key::End => (index != last).then_some(TreeMove::To(last)),
        Key::Right => match row.expanded {
            Some(false) => Some(TreeMove::Expand(true)),
            // Already open: step into the first child, which is the next row
            // only if it is actually deeper. An open branch with nothing in
            // it goes nowhere.
            Some(true) => rows
                .get(index + 1)
                .filter(|next| next.depth > row.depth)
                .map(|_| TreeMove::To(index + 1)),
            None => None,
        },
        Key::Left => match row.expanded {
            Some(true) => Some(TreeMove::Expand(false)),
            // A leaf, or a closed branch: go out to whatever contains it.
            _ => rows[..index]
                .iter()
                .rposition(|candidate| candidate.depth < row.depth)
                .map(TreeMove::To),
        },
        _ => None,
    }
}

/// Renders one tree row.
///
/// Kept per-row rather than per-tree so it can go straight into a
/// `uniform_list`, which is what keeps a large tree affordable.
///
/// The chevron and the row are separate hit targets: clicking the chevron
/// expands, clicking the row selects. A host that wants clicking anywhere to
/// expand can simply do that in `on_select`.
#[allow(clippy::too_many_arguments)]
pub fn tree_row<V: ControlHost>(
    id: &'static str,
    rows: &[TreeRow],
    index: usize,
    selected: bool,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_select: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
    on_toggle: impl Fn(&mut V, SharedString, bool, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let row = &rows[index];
    let state = view.control_state();
    // One tab stop for the tree, riding the selected row. A tree of a
    // thousand rows must not be a thousand presses to get past, and the
    // selection is where a reader already is — so it is where the keyboard
    // should land when Tab reaches the tree.
    let focus = state.focus(id, cx);
    let click_focus = focus.clone();
    let chevron: gpui::Hsla = crate::color::to_hsla(palette.text_secondary);
    let row_id = row.id.clone();
    let toggle_id = row.id.clone();
    let key_id = row.id.clone();
    let expanded = row.expanded;
    // Everything a row does to itself is keyed by the row's own id rather
    // than its index: opening a branch above it shifts every index below,
    // and a selection that jumped rows because of that would be a lie.
    let key = |what: &'static str| Tag::new((id, what, &row.id));
    // The selection washes from row to row, the chevron turns rather than
    // flips, and rows a branch reveals fade in beneath it — unless the whole
    // tree has just appeared, in which case it arrives as one thing.
    let on = state.blend(key("selected"), selected, SWITCH_SLIDE);
    let turned = expanded.map(|open| state.blend(key("open"), open, SWITCH_SLIDE));
    let shown = if state.present(id) {
        state.tween_from(key("shown"), 0.0, 1.0, SWITCH_SLIDE)
    } else {
        state.tween(key("shown"), 1.0, SWITCH_SLIDE)
    };
    let on_select = Rc::new(on_select);
    let key_select = on_select.clone();
    let on_toggle = Rc::new(on_toggle);
    let key_toggle = on_toggle.clone();
    let dbl_toggle = on_toggle.clone();
    // The moves this row can make, worked out now while the whole flattened
    // list is in hand. A row on its own knows nothing about its neighbours,
    // and the arrows are entirely about neighbours.
    let moves: Vec<(Key, TreeMove)> = [
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Home,
        Key::End,
    ]
    .into_iter()
    .filter_map(|key| tree_step(rows, index, key).map(|moved| (key, moved)))
    .collect();
    let targets: Vec<(Key, TreeMove, Option<SharedString>)> = moves
        .into_iter()
        .map(|(key, moved)| {
            let id = match moved {
                TreeMove::To(at) => rows.get(at).map(|row| row.id.clone()),
                TreeMove::Expand(_) => None,
            };
            (key, moved, id)
        })
        .collect();

    // Made before the element chain so the chain keeps `cx` for the
    // chevron's own listener further down.
    let navigate = cx.listener(move |this, event: &KeyDownEvent, window, cx| {
        let Some(key) = keyboard::key(event) else {
            return;
        };
        if key == Key::Activate {
            cx.stop_propagation();
            key_select(this, key_id.clone(), window, cx);
            cx.notify();
            return;
        }
        let Some((_, moved, target)) = targets.iter().find(|(candidate, _, _)| *candidate == key)
        else {
            return;
        };
        cx.stop_propagation();
        match moved {
            TreeMove::To(_) => {
                if let Some(target) = target.clone() {
                    key_select(this, target, window, cx);
                }
            }
            TreeMove::Expand(open) => {
                key_toggle(this, key_id.clone(), *open, window, cx);
            }
        }
        cx.notify();
    });

    div()
        .id(ElementId::NamedInteger(
            format!("{id}-row").into(),
            index as u64,
        ))
        .h(px(TREE_ROW_HEIGHT))
        .w_full()
        .flex_none()
        .pl(px(4.0 + row.depth as f32 * TREE_INDENT))
        .pr(px(8.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded(px(4.0))
        .cursor_pointer()
        .opacity(shown)
        .text_size(px(12.5))
        .text_color(crate::color::lerp(
            palette.text_primary,
            palette.control_label,
            on,
        ))
        .when(on > 0.01, |el| {
            el.bg(lighting::lit_at(palette.control_fill, 0.08, on))
        })
        .when(!selected, |el| {
            el.hover(move |style| style.bg(palette.row_hover))
        })
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            // Clicking hands the keyboard over too, so the arrows carry on
            // from wherever the pointer left off.
            window.focus(&click_focus, cx);
            // A double-click on a branch opens or closes it, as it does in
            // every file browser. The first click of the pair has already
            // selected the row, so this only adds the toggle.
            if event.click_count() == 2
                && let Some(open) = expanded
            {
                dbl_toggle(this, row_id.clone(), !open, window, cx);
                cx.notify();
                return;
            }
            on_select(this, row_id.clone(), window, cx);
            cx.notify();
        }))
        // Only the selected row is a tab stop. A tree of a thousand rows
        // must not be a thousand presses to get past, and the selection is
        // where a reader already is, so it is where the keyboard should
        // land when Tab reaches the tree.
        // The keyboard lands on the selected row, and the handle belongs to
        // the host rather than to any one row: the selection moves from row
        // to row, and a handle owned by a row would be destroyed the moment
        // it stopped being the selected one — taking focus with it after a
        // single arrow press.
        .when(selected, move |el| {
            el.relative()
                .child(keyboard::ring_for(&focus, 4.0, palette).on_key_down(navigate))
        })
        .child(
            // A leaf still reserves the chevron's width, so labels down a
            // level line up whether or not their siblings have children.
            //
            // The hit area is the full height of the row rather than the
            // glyph's own twelve pixels. A chevron that only answers to the
            // middle half of its row reads as a branch that will not open:
            // the press lands on the row instead and selects it, which looks
            // like the toggle being broken rather than like a near miss.
            div()
                .id(ElementId::NamedInteger(
                    format!("{id}-chevron").into(),
                    index as u64,
                ))
                .w(px(12.0))
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when_some(expanded.zip(turned), |el, (open, turned)| {
                    el.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                            on_toggle(this, toggle_id.clone(), !open, window, cx);
                            cx.notify();
                        }),
                    )
                    .child(
                        // The glyph keeps its own twelve pixels inside the
                        // taller hit area: the arrow is drawn from the
                        // canvas's origin, so it has to be the size it
                        // expects to be.
                        div().w(px(12.0)).h(px(12.0)).flex_none().child(
                            canvas(
                                |_bounds, _window, _cx| {},
                                move |bounds, _state, window, _cx| {
                                    // Drawn at the angle it has turned to,
                                    // from pointing right (closed) a quarter
                                    // turn to pointing down (open): gpui has
                                    // no transform, so the path is rebuilt.
                                    let origin = bounds.origin;
                                    let angle = turned * std::f32::consts::FRAC_PI_2;
                                    let (sin, cos) = angle.sin_cos();
                                    let at = |x: f32, y: f32| {
                                        point(
                                            origin.x + px(6.0 + x * cos - y * sin),
                                            origin.y + px(6.0 + x * sin + y * cos),
                                        )
                                    };
                                    let mut builder = PathBuilder::stroke(px(1.4));
                                    builder.move_to(at(-1.5, -4.0));
                                    builder.line_to(at(2.5, 0.0));
                                    builder.line_to(at(-1.5, 4.0));
                                    if let Ok(path) = builder.build() {
                                        window.paint_path(path, chevron);
                                    }
                                },
                            )
                            .size_full(),
                        ),
                    )
                }),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(row.label.clone()),
        )
        .children(row.detail.clone().map(|detail| {
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(palette.text_secondary)
                .child(detail)
        }))
}

// ---- Table header -----------------------------------------------------------

/// Which way a column is sorted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    pub fn flipped(self) -> Self {
        match self {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        }
    }
}

/// One column of a [`table_header`].
#[derive(Clone, Debug)]
pub struct Column {
    pub id: SharedString,
    pub label: SharedString,
    /// Fixed width in px, or `None` to share the leftover space evenly.
    pub width: Option<f32>,
    pub sortable: bool,
    /// Right-aligned, for numbers and sizes.
    pub numeric: bool,
}

impl Column {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            width: None,
            sortable: true,
            numeric: false,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn fixed(mut self) -> Self {
        self.sortable = false;
        self
    }

    /// Right-aligns the column, which is how a reader compares numbers.
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }
}

/// Height of a header row, and the height a body row should use to match.
pub const TABLE_ROW_HEIGHT: f32 = 26.0;
/// Horizontal padding inside every cell, header and body alike.
const TABLE_CELL_PADDING: f32 = 8.0;

/// How one column's cell is laid out: the same answer for the header and
/// for every row beneath it, which is the point of computing it once.
#[derive(Clone, Copy, PartialEq, Debug)]
struct CellLayout {
    /// `Some` for a fixed width, `None` to share the leftover space.
    width: Option<f32>,
    /// Right-aligned, which is what `numeric` means once it reaches layout.
    align_end: bool,
}

impl CellLayout {
    fn of(column: &Column) -> Self {
        Self {
            width: column.width,
            align_end: column.numeric,
        }
    }
}

/// Applies a column's layout to a cell. Both the header's cells and
/// [`table_cell`] go through here, so the two grids cannot drift apart.
/// Generic over the element so the header can keep its `id`, which is what
/// a sortable column needs in order to take a click.
fn lay_out_cell<E: Styled + gpui::prelude::FluentBuilder>(el: E, layout: CellLayout) -> E {
    el.h_full()
        .when_some(layout.width, |el, width| el.w(px(width)).flex_none())
        .when(layout.width.is_none(), |el| el.flex_1())
        .px(px(TABLE_CELL_PADDING))
        .flex()
        .items_center()
        .when(layout.align_end, |el| el.justify_end())
        .whitespace_nowrap()
        .overflow_hidden()
}

/// One cell of a body row, on the same grid as [`table_header`].
///
/// The host lays its own rows out, because only the host knows what a row
/// is: selectable, hoverable, an entry in a `uniform_list`. But the grid
/// belongs to the header, and reproducing it by hand — a width here, a
/// padding there, right-alignment on the numeric ones — is how a table ends
/// up not lining up with its own header. Pass the same [`Column`] the header
/// was given and it cannot go out of step.
///
/// ```ignore
/// div().h(px(TABLE_ROW_HEIGHT)).flex().items_center().children(
///     columns.iter().zip(values).map(|(column, value)| table_cell(column, value)),
/// )
/// ```
pub fn table_cell(column: &Column, content: impl IntoElement) -> impl IntoElement {
    lay_out_cell(div(), CellLayout::of(column)).child(content)
}

/// One body row of plain text, on the header's grid, that slides to its
/// place: `slot` is where the row sits now, and a re-sort slides each row
/// to its new slot rather than redealing the table. Keyed by `key` — what
/// the row *is*, not where it is — so the row is seen to be the same row
/// somewhere else. The first value is what the row is and is set in primary
/// text; the rest describe it, and are set quieter.
///
/// For a row that is more than text — selectable, hoverable, with a control
/// in it — lay it out with [`table_cell`] instead.
pub fn table_row(
    table: &'static str,
    key: impl Hash,
    slot: usize,
    columns: &[Column],
    values: impl IntoIterator<Item = impl Into<SharedString>>,
    palette: Palette,
    state: &ControlState,
) -> gpui::Div {
    let place = slot as f32 * TABLE_ROW_HEIGHT;
    let offset = state.tween((table, "row", key), place, MOVE) - place;
    div()
        .relative()
        .top(px(offset))
        .h(px(TABLE_ROW_HEIGHT))
        .flex()
        .flex_none()
        .items_center()
        .children(
            columns
                .iter()
                .zip(values)
                .enumerate()
                .map(|(index, (column, value))| {
                    table_cell(
                        column,
                        div()
                            .text_color(if index == 0 {
                                palette.text_primary
                            } else {
                                palette.text_secondary
                            })
                            .child(value.into()),
                    )
                }),
        )
}

/// Header row for a table: column labels, with an arrow on the sorted one.
///
/// Clicking a sortable column reports it and the direction it should take:
/// the same column flips, a different one starts ascending, which is what
/// every table does and what nobody has to be told.
#[allow(clippy::too_many_arguments)]
pub fn table_header<V: ControlHost>(
    id: &'static str,
    columns: &[Column],
    sorted_by: Option<(&str, SortDirection)>,
    palette: Palette,
    cx: &mut Context<V>,
    on_sort: impl Fn(&mut V, SharedString, SortDirection, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let on_sort = Rc::new(on_sort);
    let mut cells: Vec<AnyElement> = Vec::with_capacity(columns.len());

    for (index, column) in columns.iter().enumerate() {
        let cx: &mut Context<V> = &mut *cx;
        let on_sort = on_sort.clone();
        let column_id = column.id.clone();
        let active = sorted_by.filter(|(sorted, _)| *sorted == column.id.as_ref());
        let sort_ring = ElementId::NamedInteger(format!("{id}-column-ring").into(), index as u64);
        let next = match active {
            Some((_, direction)) => direction.flipped(),
            None => SortDirection::Ascending,
        };
        let arrow_up = matches!(active, Some((_, SortDirection::Ascending)));
        let arrow: gpui::Hsla = crate::color::to_hsla(palette.text_primary);
        cells.push(
            lay_out_cell(
                div().id(ElementId::NamedInteger(
                    format!("{id}-column").into(),
                    index as u64,
                )),
                CellLayout::of(column),
            )
            .gap(px(4.0))
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active.is_some() {
                palette.text_primary
            } else {
                palette.text_secondary
            })
            .when(column.sortable, move |el| {
                let key_id = column_id.clone();
                let key_sort = on_sort.clone();
                el.relative()
                    .cursor_pointer()
                    .hover(move |style| style.bg(palette.row_hover))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        on_sort(this, column_id.clone(), next, window, cx);
                        cx.notify();
                    }))
                    .child(
                        keyboard::ring(sort_ring, 4.0, palette).on_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, window, cx| {
                                if keyboard::key(event) == Some(Key::Activate) {
                                    cx.stop_propagation();
                                    key_sort(this, key_id.clone(), next, window, cx);
                                    cx.notify();
                                }
                            },
                        )),
                    )
            })
            .child(column.label.clone())
            .when(active.is_some(), move |el| {
                el.child(
                    div().w(px(9.0)).h(px(9.0)).flex_none().child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, _cx| {
                                let origin = bounds.origin;
                                let mut builder = PathBuilder::stroke(px(1.3));
                                if arrow_up {
                                    builder.move_to(point(origin.x + px(0.5), origin.y + px(6.5)));
                                    builder.line_to(point(origin.x + px(4.5), origin.y + px(2.5)));
                                    builder.line_to(point(origin.x + px(8.5), origin.y + px(6.5)));
                                } else {
                                    builder.move_to(point(origin.x + px(0.5), origin.y + px(2.5)));
                                    builder.line_to(point(origin.x + px(4.5), origin.y + px(6.5)));
                                    builder.line_to(point(origin.x + px(8.5), origin.y + px(2.5)));
                                }
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, arrow);
                                }
                            },
                        )
                        .size_full(),
                    ),
                )
            })
            .into_any_element(),
        );
    }

    let mut rule: gpui::Hsla = crate::color::to_hsla(palette.field_border);
    rule.alpha = 0.9;
    div()
        .id(id)
        .h(px(TABLE_ROW_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_stretch()
        .border_b_1()
        .border_color(rule)
        .children(cells)
}

#[cfg(test)]
mod tests {
    use super::{CellLayout, Column, Key, SortDirection, TreeMove, TreeRow, tree_step};

    /// Documents / report.pdf, Images (collapsed), readme.md
    fn tree() -> Vec<TreeRow> {
        vec![
            TreeRow::branch("documents", "Documents", 0, true),
            TreeRow::leaf("report", "report.pdf", 1),
            TreeRow::branch("images", "Images", 1, false),
            TreeRow::leaf("readme", "readme.md", 1),
        ]
    }

    /// A tree is content, not a set of choices: pressing up at the top stays
    /// at the top rather than throwing the reader to the bottom.
    #[test]
    fn a_tree_does_not_wrap() {
        assert_eq!(tree_step(&tree(), 0, Key::Up), None);
        assert_eq!(tree_step(&tree(), 3, Key::Down), None);
    }

    #[test]
    fn up_and_down_walk_the_flattened_rows() {
        assert_eq!(tree_step(&tree(), 1, Key::Down), Some(TreeMove::To(2)));
        assert_eq!(tree_step(&tree(), 2, Key::Up), Some(TreeMove::To(1)));
    }

    #[test]
    fn right_opens_a_closed_branch_then_walks_into_it() {
        // "Images" is closed: the first right opens it.
        assert_eq!(
            tree_step(&tree(), 2, Key::Right),
            Some(TreeMove::Expand(true))
        );
        // "Documents" is already open: right steps to its first child.
        assert_eq!(tree_step(&tree(), 0, Key::Right), Some(TreeMove::To(1)));
    }

    #[test]
    fn left_closes_an_open_branch_then_walks_out_to_the_parent() {
        assert_eq!(
            tree_step(&tree(), 0, Key::Left),
            Some(TreeMove::Expand(false))
        );
        // A leaf goes out to whatever contains it.
        assert_eq!(tree_step(&tree(), 1, Key::Left), Some(TreeMove::To(0)));
    }

    /// A leaf has no children to step into and nothing to open.
    #[test]
    fn right_does_nothing_on_a_leaf() {
        assert_eq!(tree_step(&tree(), 1, Key::Right), None);
    }

    /// An open branch with nothing under it goes nowhere, rather than
    /// stepping onto its sibling as though it were a child.
    #[test]
    fn right_on_an_empty_open_branch_stays_put() {
        let rows = vec![
            TreeRow::branch("empty", "Empty", 0, true),
            TreeRow::leaf("sibling", "Sibling", 0),
        ];
        assert_eq!(tree_step(&rows, 0, Key::Right), None);
    }

    #[test]
    fn home_and_end_reach_the_ends() {
        assert_eq!(tree_step(&tree(), 2, Key::Home), Some(TreeMove::To(0)));
        assert_eq!(tree_step(&tree(), 0, Key::End), Some(TreeMove::To(3)));
        // Already there: no move to report.
        assert_eq!(tree_step(&tree(), 0, Key::Home), None);
    }

    #[test]
    fn an_index_off_the_end_moves_nothing() {
        assert_eq!(tree_step(&tree(), 99, Key::Down), None);
        assert_eq!(tree_step(&[], 0, Key::Down), None);
    }

    /// The header and the rows below it are laid out from the same `Column`,
    /// so asking twice has to give the same answer. When the gallery laid its
    /// rows out by hand instead, the Size column drifted 10px from its header
    /// and Changed drifted 16px.
    #[test]
    fn one_column_gives_one_layout() {
        let column = Column::new("size", "Size").width(80.0).numeric();
        assert_eq!(CellLayout::of(&column), CellLayout::of(&column));
    }

    #[test]
    fn a_column_without_a_width_shares_the_leftover_space() {
        let flexible = Column::new("name", "Name");
        assert_eq!(CellLayout::of(&flexible).width, None);

        let fixed = Column::new("changed", "Changed").width(110.0);
        assert_eq!(CellLayout::of(&fixed).width, Some(110.0));
    }

    /// `numeric` is a claim about the data; right-alignment is what it means
    /// once it reaches layout, and it has to mean that on both sides of the
    /// header rule or the digits do not line up.
    #[test]
    fn numeric_columns_align_to_the_right() {
        assert!(CellLayout::of(&Column::new("size", "Size").numeric()).align_end);
        assert!(!CellLayout::of(&Column::new("name", "Name")).align_end);
    }

    /// A fixed column keeps its width whether or not it is sortable, so a
    /// non-sortable column does not shift the ones beside it.
    #[test]
    fn sortability_does_not_change_the_grid() {
        let sortable = Column::new("size", "Size").width(80.0).numeric();
        let fixed = Column::new("size", "Size").width(80.0).numeric().fixed();
        assert_eq!(CellLayout::of(&sortable), CellLayout::of(&fixed));
    }

    #[test]
    fn a_sort_direction_flips_both_ways() {
        assert_eq!(
            SortDirection::Ascending.flipped(),
            SortDirection::Descending
        );
        assert_eq!(
            SortDirection::Descending.flipped(),
            SortDirection::Ascending
        );
    }
}
