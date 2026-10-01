//! A selectable list and the host-owned selection it edits.
//!
//! The toolkit handles the same mouse and keyboard gestures in every list;
//! the host keeps the chosen ids alongside the data they describe.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Div, ElementId, KeyDownEvent, Modifiers, Rgba, SharedString, Window, div,
    prelude::*, px,
};

use crate::controls::{WidgetContext, scrollbar};
use crate::keyboard::{self, Dismiss, Key};
use crate::lighting;
use crate::scroll::{ScrollAxis, scroll_fades};
use crate::state::{ComboId, ControlHost, SWITCH_SLIDE};

/// A list gesture, with the item identity rather than its changing position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionIntent {
    Choose {
        id: SharedString,
        extend: bool,
        toggle: bool,
    },
    All,
    Clear,
    Activate(SharedString),
}

type OnIntent<V> =
    Rc<dyn Fn(&mut V, &[SharedString], SelectionIntent, &mut Window, &mut Context<V>)>;

/// Cmd on macOS, Ctrl elsewhere, with nothing but Shift beside it. Gpui's
/// `Modifiers::secondary` alone would also take Cmd-Ctrl or Cmd-Option,
/// which are someone else's shortcuts rather than a toggle.
fn secondary_only(modifiers: &Modifiers) -> bool {
    Modifiers {
        shift: false,
        ..*modifiers
    } == Modifiers::secondary_key()
}

fn position(order: &[SharedString], id: &SharedString) -> Option<usize> {
    order.iter().position(|candidate| candidate == id)
}

/// The row the ring is drawn on: the active one, or the first while no
/// active row is in view, so a fresh or filtered list is still one Tab away.
fn active_index(order: &[SharedString], active: Option<&SharedString>) -> usize {
    active.and_then(|id| position(order, id)).unwrap_or(0)
}

fn navigation_target(key: Key, index: usize, count: usize) -> Option<usize> {
    match key {
        Key::Up => index.checked_sub(1),
        Key::Down => (index + 1 < count).then_some(index + 1),
        Key::Home => (count > 0).then_some(0),
        Key::End => count.checked_sub(1),
        _ => None,
    }
}

/// A vertically scrolling list of selectable rows, one for each id in
/// `order`, with `rows` as their content in the same order.
///
/// Only the active row gets the list's focus handle, so Tab crosses even a
/// very long list in one step. Click, Cmd/Ctrl-click and Shift-click are
/// reported as selection intents; Up/Down/Home/End, Shift-arrows,
/// Cmd/Ctrl+A, Enter and Escape use the same callback, and the arrows
/// scroll their target into view. `on_intent` is handed the order the rows
/// were drawn in, so the host applies the intent to its [`ListSelection`]
/// without keeping a copy for the listener, and decides what activating an
/// item does. Escape with nothing selected goes on to the root.
///
/// `start` and `end` are the colours behind the list at its top and bottom
/// edges, as for [`scroll_fades`]: the fades have to land on them exactly.
/// Give the returned element a height so a long list has a viewport to
/// scroll in.
///
/// ```ignore
/// let (top, bottom) = lit_stops(palette.area_surface, CARD_LIFT);
/// selectable_list(
///     "files", &order, &self.selection, top, bottom,
///     WidgetContext::new(palette, self, cx),
///     order.iter().map(|name| div().child(name.clone())),
///     |this, order, intent, _window, _cx| this.selection.apply(order, intent),
/// )
/// .h(px(200.0))
/// ```
#[allow(clippy::too_many_arguments)]
pub fn selectable_list<V: ControlHost, E: IntoElement>(
    id: ComboId,
    order: &[SharedString],
    selection: &ListSelection,
    start: Rgba,
    end: Rgba,
    mut ctx: WidgetContext<'_, '_, '_, V>,
    rows: impl IntoIterator<Item = E>,
    on_intent: impl Fn(&mut V, &[SharedString], SelectionIntent, &mut Window, &mut Context<V>) + 'static,
) -> Div {
    let scroll = ctx.state().scroll((id, "selection-scroll"));
    // One copy of the order per frame, shared by every row's listeners.
    let order: Rc<[SharedString]> = order.into();
    let on_intent: OnIntent<V> = Rc::new(on_intent);
    let active = active_index(&order, selection.active());
    let mut list = div()
        .id(ElementId::Name(format!("{id}-selection-scroll").into()))
        .size_full()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .overflow_y_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(&scroll);
    for (index, content) in rows.into_iter().take(order.len()).enumerate() {
        list = list.child(selectable_row(
            id,
            &order,
            index,
            index == active,
            selection,
            content,
            ctx.reborrow(),
            on_intent.clone(),
        ));
    }
    div()
        .relative()
        .overflow_hidden()
        .child(list)
        .children(scroll_fades(
            ctx.state(),
            id,
            &scroll,
            ScrollAxis::Vertical,
            start,
            end,
        ))
        .child(scrollbar(id, &scroll, ScrollAxis::Vertical, ctx))
}

/// The host-owned selection for a list of stable ids.
///
/// `active` is the row carrying the list's one keyboard focus handle.
/// `anchor` stays put while Shift extends the selection from it.
#[derive(Clone, Debug, Default)]
pub struct ListSelection {
    selected: HashSet<SharedString>,
    active: Option<SharedString>,
    anchor: Option<SharedString>,
}

impl ListSelection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.selected.contains(id)
    }

    /// How many ids are selected, including any no longer shown until
    /// [`retain_visible`](Self::retain_visible) drops them.
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn active(&self) -> Option<&SharedString> {
        self.active.as_ref()
    }

    pub fn selected_in(&self, order: &[SharedString]) -> Vec<SharedString> {
        order
            .iter()
            .filter(|id| self.selected.contains(*id))
            .cloned()
            .collect()
    }

    /// Reconciles a selection with the rows still shown after filtering.
    pub fn retain_visible(&mut self, order: &[SharedString]) {
        let visible: HashSet<_> = order.iter().cloned().collect();
        self.selected.retain(|id| visible.contains(id));
        if self.active.as_ref().is_some_and(|id| !visible.contains(id)) {
            self.active = order.first().cloned();
        }
        if self.anchor.as_ref().is_some_and(|id| !visible.contains(id)) {
            self.anchor = self.active.clone();
        }
    }

    pub fn apply(&mut self, order: &[SharedString], intent: SelectionIntent) {
        match intent {
            SelectionIntent::Choose { id, extend, toggle } => {
                let Some(at) = position(order, &id) else {
                    return;
                };
                if extend {
                    // Without an anchor — a fresh list, or after Escape — a
                    // range starts from the row the ring is on, which is
                    // where the reader's eye already is. Starting from the
                    // target instead would make Shift-Down select one row.
                    let start = self
                        .anchor
                        .as_ref()
                        .and_then(|anchor| position(order, anchor))
                        .unwrap_or_else(|| active_index(order, self.active.as_ref()));
                    if !toggle {
                        self.selected.clear();
                    }
                    for row in &order[start.min(at)..=start.max(at)] {
                        self.selected.insert(row.clone());
                    }
                    self.anchor = Some(order[start].clone());
                } else if toggle {
                    if !self.selected.insert(id.clone()) {
                        self.selected.remove(&id);
                    }
                    self.anchor = Some(id.clone());
                } else {
                    self.selected.clear();
                    self.selected.insert(id.clone());
                    self.anchor = Some(id.clone());
                }
                self.active = Some(id);
            }
            SelectionIntent::All => {
                self.selected = order.iter().cloned().collect();
                if self.active.as_ref().is_none_or(|id| !order.contains(id)) {
                    self.active = order.first().cloned();
                }
                self.anchor = self.active.clone();
            }
            SelectionIntent::Clear => {
                self.selected.clear();
                self.anchor = None;
            }
            SelectionIntent::Activate(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn selectable_row<V: ControlHost>(
    id: ComboId,
    order: &Rc<[SharedString]>,
    index: usize,
    active: bool,
    selection: &ListSelection,
    content: impl IntoElement,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_intent: OnIntent<V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let row_id = order[index].clone();
    let selected = selection.contains(&row_id);
    let has_selection = !selection.is_empty();
    let focus = view.control_state().focus(id, cx);
    let click_focus = focus.clone();
    let on = view
        .control_state()
        .blend((id, "selected", &row_id), selected, SWITCH_SLIDE);
    let click_intent = on_intent.clone();
    let click_order = order.clone();
    let dismiss_intent = on_intent.clone();
    let dismiss_order = order.clone();
    let key_order = order.clone();

    div()
        .id(ElementId::NamedChild(
            Arc::new(ElementId::Name(id.into())),
            row_id.clone(),
        ))
        .relative()
        .min_w(px(0.0))
        .w_full()
        .rounded(px(crate::controls::CONTROL_RADIUS))
        .cursor_pointer()
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
            window.focus(&click_focus, cx);
            let modifiers = event.modifiers();
            let intent = if event.click_count() == 2 && !modifiers.modified() {
                SelectionIntent::Activate(row_id.clone())
            } else {
                SelectionIntent::Choose {
                    id: row_id.clone(),
                    extend: modifiers.shift,
                    toggle: secondary_only(&modifiers),
                }
            };
            click_intent(this, &click_order, intent, window, cx);
            cx.notify();
        }))
        .child(content)
        .when(active, move |el| {
            el.child(
                keyboard::ring_for(&focus, crate::controls::CONTROL_RADIUS, palette)
                    .on_action(cx.listener(move |this, _: &Dismiss, window, cx| {
                        // Nothing selected means nothing here for Escape to
                        // undo, so the root gets it and closes an overlay,
                        // as it would with the keyboard anywhere else.
                        if !has_selection {
                            cx.propagate();
                            return;
                        }
                        dismiss_intent(this, &dismiss_order, SelectionIntent::Clear, window, cx);
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let modifiers = &event.keystroke.modifiers;
                        let (intent, scroll_to) = if *modifiers == Modifiers::secondary_key()
                            && event.keystroke.key == "a"
                        {
                            (SelectionIntent::All, None)
                        } else {
                            match keyboard::key(event) {
                                Some(Key::Activate) => {
                                    (SelectionIntent::Activate(key_order[index].clone()), None)
                                }
                                Some(key) => {
                                    let Some(at) = navigation_target(key, index, key_order.len())
                                    else {
                                        return;
                                    };
                                    (
                                        SelectionIntent::Choose {
                                            id: key_order[at].clone(),
                                            extend: modifiers.shift,
                                            toggle: false,
                                        },
                                        Some(at),
                                    )
                                }
                                None => return,
                            }
                        };
                        cx.stop_propagation();
                        if let Some(at) = scroll_to {
                            this.control_state()
                                .scroll((id, "selection-scroll"))
                                .scroll_to_item(at);
                        }
                        on_intent(this, &key_order, intent, window, cx);
                        cx.notify();
                    })),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Vec<SharedString> {
        ["a", "b", "c", "d"].into_iter().map(Into::into).collect()
    }

    #[test]
    fn plain_toggle_and_shift_choose_stable_ids() {
        let order = order();
        let mut selection = ListSelection::new();
        selection.apply(&order, choose("b", false, false));
        selection.apply(&order, choose("d", false, true));
        assert_eq!(selection.selected_in(&order), vec!["b", "d"]);
        selection.apply(&order, choose("b", true, false));
        assert_eq!(selection.selected_in(&order), vec!["b", "c", "d"]);
        selection.apply(&order, choose("c", false, false));
        assert_eq!(selection.selected_in(&order), vec!["c"]);
    }

    #[test]
    fn filtering_rehomes_active_and_discards_hidden_ids() {
        let order = order();
        let mut selection = ListSelection::new();
        selection.apply(&order, SelectionIntent::All);
        selection.apply(
            &order,
            SelectionIntent::Choose {
                id: "d".into(),
                extend: false,
                toggle: true,
            },
        );
        let visible = vec!["a".into(), "c".into()];
        selection.retain_visible(&visible);
        assert_eq!(selection.active().map(SharedString::as_ref), Some("a"));
        assert_eq!(selection.selected_in(&visible), visible);
    }

    #[test]
    fn a_filtered_out_active_id_leaves_focus_on_the_first_visible_row() {
        let visible = vec!["a".into(), "c".into()];
        let stale: SharedString = "b".into();
        assert_eq!(active_index(&visible, Some(&stale)), 0);
        assert_eq!(active_index(&visible, None), 0);
        assert_eq!(active_index(&visible, Some(&visible[1])), 1);
    }

    fn choose(id: &str, extend: bool, toggle: bool) -> SelectionIntent {
        SelectionIntent::Choose {
            id: id.into(),
            extend,
            toggle,
        }
    }

    #[test]
    fn shift_after_escape_extends_from_the_active_row() {
        let order = order();
        let mut selection = ListSelection::new();
        selection.apply(&order, choose("b", false, false));
        selection.apply(&order, SelectionIntent::Clear);
        assert!(selection.is_empty());
        selection.apply(&order, choose("c", true, false));
        assert_eq!(selection.selected_in(&order), vec!["b", "c"]);
        // The range keeps pivoting on where it started.
        selection.apply(&order, choose("d", true, false));
        assert_eq!(selection.selected_in(&order), vec!["b", "c", "d"]);
    }

    #[test]
    fn shift_on_a_fresh_list_extends_from_the_first_row() {
        let order = order();
        let mut selection = ListSelection::new();
        selection.apply(&order, choose("b", true, false));
        assert_eq!(selection.selected_in(&order), vec!["a", "b"]);
        selection.apply(&order, choose("c", true, false));
        assert_eq!(selection.selected_in(&order), vec!["a", "b", "c"]);
    }

    #[test]
    fn secondary_shift_adds_a_range_to_the_selection() {
        let order = order();
        let mut selection = ListSelection::new();
        selection.apply(&order, choose("a", false, false));
        selection.apply(&order, choose("c", false, true));
        selection.apply(&order, choose("d", true, true));
        assert_eq!(selection.selected_in(&order), vec!["a", "c", "d"]);
        assert_eq!(selection.len(), 3);
    }

    #[test]
    fn select_all_on_an_empty_list_selects_nothing() {
        let mut selection = ListSelection::new();
        selection.apply(&[], SelectionIntent::All);
        assert!(selection.is_empty());
        assert_eq!(selection.active(), None);
    }

    #[test]
    fn choosing_an_id_not_in_the_list_changes_nothing() {
        let order = order();
        let mut selection = ListSelection::new();
        selection.apply(&order, choose("b", false, false));
        selection.apply(&order, choose("z", false, false));
        selection.apply(&order, choose("z", true, true));
        assert_eq!(selection.selected_in(&order), vec!["b"]);
        assert_eq!(selection.active().map(SharedString::as_ref), Some("b"));
    }

    #[test]
    fn only_the_platform_key_and_shift_toggle() {
        let secondary = Modifiers::secondary_key();
        assert!(secondary_only(&secondary));
        assert!(secondary_only(&Modifiers {
            shift: true,
            ..secondary
        }));
        assert!(!secondary_only(&Modifiers {
            alt: true,
            ..secondary
        }));
        assert!(!secondary_only(&Modifiers::none()));
    }

    #[test]
    fn scroll_targets_follow_navigation_without_wrapping() {
        assert_eq!(navigation_target(Key::Down, 0, 12), Some(1));
        assert_eq!(navigation_target(Key::End, 0, 12), Some(11));
        assert_eq!(navigation_target(Key::Up, 11, 12), Some(10));
        assert_eq!(navigation_target(Key::Home, 11, 12), Some(0));
        assert_eq!(navigation_target(Key::Up, 0, 12), None);
        assert_eq!(navigation_target(Key::Down, 11, 12), None);
        assert_eq!(navigation_target(Key::End, 0, 0), None);
    }
}
