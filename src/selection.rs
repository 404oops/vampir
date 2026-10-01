//! A selectable list row and the host-owned selection it edits.
//!
//! The toolkit handles the same mouse and keyboard gestures in every list;
//! the host keeps the chosen ids alongside the data they describe.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, KeyDownEvent, Modifiers, SharedString, Window,
    div, prelude::*, px,
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

fn primary_modifier(modifiers: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control
    } else {
        modifiers.control && !modifiers.platform
    }
}

fn active_row(order: &[SharedString], index: usize, active: Option<&SharedString>) -> bool {
    active == order.get(index) || (index == 0 && active.is_none_or(|id| !order.contains(id)))
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

/// A vertically scrolling viewport for [`selectable_row`]s with the same id.
///
/// Pass the rows as direct children, in the same order used to build each
/// row. The tracked scroll handle then brings an arrow-key target into view
/// without the host routing keys or keeping a second scroll model. Give the
/// returned element a height so a long list has a viewport to scroll in.
pub fn selectable_list<V: ControlHost>(
    id: ComboId,
    rows: impl IntoIterator<Item = AnyElement>,
    mut ctx: WidgetContext<'_, '_, '_, V>,
) -> Div {
    let palette = ctx.palette;
    let scroll = ctx.state().scroll((id, "selection-scroll"));
    div()
        .relative()
        .overflow_hidden()
        .child(
            div()
                .id(ElementId::Name(format!("{id}-selection-scroll").into()))
                .size_full()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .overflow_y_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&scroll)
                .children(rows),
        )
        .children(scroll_fades(
            ctx.state(),
            id,
            &scroll,
            ScrollAxis::Vertical,
            palette.area_surface,
            palette.area_surface,
        ))
        .child(scrollbar(id, &scroll, ScrollAxis::Vertical, ctx.reborrow()))
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
                let Some(at) = order.iter().position(|candidate| candidate == &id) else {
                    return;
                };
                if extend {
                    let start = self
                        .anchor
                        .as_ref()
                        .and_then(|anchor| order.iter().position(|candidate| candidate == anchor))
                        .unwrap_or(at);
                    if !toggle {
                        self.selected.clear();
                    }
                    for row in &order[start.min(at)..=start.max(at)] {
                        self.selected.insert(row.clone());
                    }
                    if self.anchor.is_none() {
                        self.anchor = Some(id.clone());
                    }
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

/// One row of a host-owned list, with an arbitrary element as its content.
///
/// Only the active row gets the list's focus handle, so Tab crosses even a
/// very long list in one step. Click, Cmd/Ctrl-click and Shift-click are
/// reported as selection intents; Up/Down/Home/End, Shift-arrows, Cmd/Ctrl+A,
/// Enter and Escape use the same callback. The host applies the intent to a
/// [`ListSelection`] and decides what activating an item does.
#[allow(clippy::too_many_arguments)]
pub fn selectable_row<V: ControlHost>(
    id: ComboId,
    order: &[SharedString],
    index: usize,
    selection: &ListSelection,
    content: impl IntoElement,
    ctx: WidgetContext<'_, '_, '_, V>,
    on_intent: impl Fn(&mut V, SelectionIntent, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    let row_id = order[index].clone();
    let selected = selection.contains(&row_id);
    let active = active_row(order, index, selection.active());
    let focus = view.control_state().focus(id, cx);
    let click_focus = focus.clone();
    let on = view
        .control_state()
        .blend((id, "selected", &row_id), selected, SWITCH_SLIDE);
    let on_intent = Rc::new(on_intent);
    let click_intent = on_intent.clone();
    let key_intent = on_intent.clone();
    let dismiss_intent = on_intent.clone();
    let previous = index.checked_sub(1).and_then(|at| order.get(at)).cloned();
    let next = order.get(index + 1).cloned();
    let first = order.first().cloned();
    let last = order.last().cloned();
    let count = order.len();
    let current = row_id.clone();

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
                    toggle: primary_modifier(&modifiers) && !modifiers.alt && !modifiers.function,
                }
            };
            click_intent(this, intent, window, cx);
            cx.notify();
        }))
        .child(content)
        .when(active, move |el| {
            el.child(
                keyboard::ring_for(&focus, crate::controls::CONTROL_RADIUS, palette)
                    .on_action(cx.listener(move |this, _: &Dismiss, window, cx| {
                        dismiss_intent(this, SelectionIntent::Clear, window, cx);
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let modifiers = &event.keystroke.modifiers;
                        let intent = if primary_modifier(modifiers)
                            && !modifiers.shift
                            && !modifiers.alt
                            && !modifiers.function
                            && event.keystroke.key == "a"
                        {
                            Some((SelectionIntent::All, None))
                        } else if let Some(key) = keyboard::key(event) {
                            let target_id = match key {
                                Key::Up => previous.clone(),
                                Key::Down => next.clone(),
                                Key::Home => first.clone(),
                                Key::End => last.clone(),
                                Key::Activate => {
                                    cx.stop_propagation();
                                    key_intent(
                                        this,
                                        SelectionIntent::Activate(current.clone()),
                                        window,
                                        cx,
                                    );
                                    cx.notify();
                                    return;
                                }
                                _ => None,
                            };
                            navigation_target(key, index, count)
                                .zip(target_id)
                                .map(|(at, id)| {
                                    (
                                        SelectionIntent::Choose {
                                            id,
                                            extend: modifiers.shift,
                                            toggle: false,
                                        },
                                        Some(at),
                                    )
                                })
                        } else {
                            None
                        };
                        if let Some((intent, scroll_to)) = intent {
                            cx.stop_propagation();
                            if let Some(at) = scroll_to {
                                this.control_state()
                                    .scroll((id, "selection-scroll"))
                                    .scroll_to_item(at);
                            }
                            key_intent(this, intent, window, cx);
                            cx.notify();
                        }
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
        let choose = |id: &str, extend, toggle| SelectionIntent::Choose {
            id: id.into(),
            extend,
            toggle,
        };
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
        assert!(active_row(&visible, 0, Some(&stale)));
        assert!(!active_row(&visible, 1, Some(&stale)));
        assert!(active_row(&visible, 1, Some(&visible[1])));
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
