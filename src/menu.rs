//! Right-click context menus.
//!
//! A menu is opened from wherever the press happened, by putting it in the
//! host's [`ControlState`], and rendered once near the root of the view.
//! Which items it holds is the host's decision each frame, because that
//! depends on what was clicked, which the state carries as an opaque target
//! string.
//!
//! ```ignore
//! // On the row, in an on_mouse_down(MouseButton::Right) listener:
//! this.control_state_mut().open_menu("row", event.position, row_id.to_string());
//!
//! // Once, near the root of render:
//! .children(vampir::context_menu("row", &items, palette, self, cx,
//!     |host, item, _window, cx| host.run_menu_item(item, cx)))
//! ```

use std::rc::Rc;
use std::time::Instant;

use gpui::{
    Anchor, App, ElementId, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, SharedString,
    Window, anchored, canvas, deferred, div, point, prelude::*, px,
};

use crate::keyboard::{self, Key, Orientation};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{ComboId, ControlHost};

/// Metrics chosen so a menu of ordinary items lines up with the pop-up list
/// a [`crate::controls::combo`] drops, which is the same thing seen from a
/// different angle.
const ITEM_HEIGHT: f32 = 26.0;
const MENU_PADDING: f32 = 5.0;
const MENU_RADIUS: f32 = 7.0;
const MENU_MIN_WIDTH: f32 = 168.0;
/// Kept clear of the window edges, so a menu summoned in a corner still
/// reads as floating above the content rather than welded to the frame.
const VIEWPORT_MARGIN: f32 = 8.0;

/// One row of a context menu.
#[derive(Clone, Debug)]
pub enum MenuItem {
    Action(MenuAction),
    /// A hairline between groups.
    Separator,
    /// A quiet label naming the group below it.
    Header(SharedString),
}

/// A menu row that does something when clicked.
#[derive(Clone, Debug)]
pub struct MenuAction {
    /// Comes back to the activation callback. Also the element id, so no two
    /// actions in one menu may share it.
    pub id: ComboId,
    pub label: SharedString,
    /// Right-aligned accelerator text, if the action has one.
    pub shortcut: Option<SharedString>,
    pub enabled: bool,
    /// Draws a tick in the gutter, for a menu that toggles something.
    pub checked: bool,
    /// Destructive: tinted, and always last in its group.
    pub danger: bool,
}

impl MenuItem {
    /// An enabled, unchecked, ordinary action.
    pub fn action(id: ComboId, label: impl Into<SharedString>) -> Self {
        MenuItem::Action(MenuAction {
            id,
            label: label.into(),
            shortcut: None,
            enabled: true,
            checked: false,
            danger: false,
        })
    }

    pub fn header(label: impl Into<SharedString>) -> Self {
        MenuItem::Header(label.into())
    }

    pub fn separator() -> Self {
        MenuItem::Separator
    }

    /// Right-aligned accelerator text. Purely a label: binding the key is
    /// the host's business.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        if let MenuItem::Action(action) = &mut self {
            action.shortcut = Some(shortcut.into());
        }
        self
    }

    /// Greys the row out and stops it taking clicks. Prefer this to dropping
    /// the item: a menu whose rows move between openings is hard to use.
    pub fn disabled(mut self) -> Self {
        if let MenuItem::Action(action) = &mut self {
            action.enabled = false;
        }
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        if let MenuItem::Action(action) = &mut self {
            action.checked = checked;
        }
        self
    }

    pub fn danger(mut self) -> Self {
        if let MenuItem::Action(action) = &mut self {
            action.danger = true;
        }
        self
    }
}

/// Renders the context menu `id` if it is the one currently open, otherwise
/// `None`. Put the result near the root of the view: it positions itself in
/// window coordinates and floats above everything.
///
/// Clicking an item runs `on_activate` with the item's id and then closes
/// the menu; pressing anywhere else closes it without running anything.
///
/// The order matters. The menu is still open while `on_activate` runs, so
/// [`ControlState::menu_target`](crate::ControlState::menu_target) still
/// answers with whatever the menu was summoned on — which is the only
/// moment that answer is any use. A callback that opens a menu of its own
/// keeps it; only the opening that was clicked is closed.
#[allow(clippy::too_many_arguments)]
pub fn context_menu<V: ControlHost>(
    id: ComboId,
    items: &[MenuItem],
    palette: Palette,
    view: &mut V,
    cx: &mut Context<V>,
    on_activate: impl Fn(&mut V, ComboId, &mut Window, &mut Context<V>) + 'static,
) -> Option<impl IntoElement> {
    // Copied out rather than borrowed: the focus handle below needs the
    // state mutably, and the menu's own fields are small.
    let (anchor, under, opened_at, highlight) = {
        let menu = view.control_state().menu.as_ref()?;
        if menu.id != id {
            return None;
        }
        (menu.anchor, menu.under, menu.opened_at, menu.highlight)
    };
    let dark = palette.is_dark;
    let on_activate: Activate<V> = Rc::new(on_activate);
    let on_key_activate = on_activate.clone();
    let items = items.to_vec();
    let focus = view.control_state_mut().menu_focus(cx);
    let focus_on_open = focus.clone();
    let weak = cx.entity().downgrade();

    // A menu that would run off the window is flipped back over the
    // pointer, which is what every desktop toolkit does and what the hand
    // already expects. `anchored` measures the real panel to decide, so a
    // long label is accounted for; this only has to say which corner sits
    // at the pointer and how much room to leave at the edges.
    let at = under
        .map(|under| under.bottom_left() + point(px(0.0), px(4.0)))
        .unwrap_or(anchor);

    // Built up front: each row needs `cx` to make its listener, and a
    // closure passed to `map` cannot hand the same `&mut` out more than
    // once.
    // Identifies this opening, so an item can close the menu it was in
    // without closing one the host's callback opened in its place.

    // Only enabled actions can be arrowed onto: a menu whose arrows stop on
    // a separator, or on a row that refuses to run, reads as broken.
    let reachable: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, MenuItem::Action(action) if action.enabled))
        .map(|(index, _)| index)
        .collect();
    let reachable_ids: Vec<ComboId> = reachable
        .iter()
        .filter_map(|index| match &items[*index] {
            MenuItem::Action(action) => Some(action.id),
            _ => None,
        })
        .collect();

    // The panel fades in over the same reveal a pop-up list uses, so the
    // two kinds of thing that drop out of a click arrive the same way.
    let reveal = crate::easing::ease_out_cubic(crate::easing::progress(
        opened_at,
        view.control_state().scaled(crate::state::COMBO_REVEAL),
    ));

    let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let lit = highlight.is_some_and(|at| reachable.get(at) == Some(&index));
        // The keyboard's wash moves from row to row rather than jumping.
        let lit = view.control_state().blend(
            &ElementId::NamedInteger(format!("{id}-menu-lit").into(), index as u64),
            lit,
            crate::state::SWITCH_SLIDE,
        );
        rows.push(render_item(
            index,
            item,
            lit,
            opened_at,
            palette,
            cx,
            &on_activate,
        ));
    }

    Some(
        deferred(
            anchored()
                .position(at)
                .anchor(Anchor::TopLeft)
                .snap_to_window_with_margin(px(VIEWPORT_MARGIN))
                .child(
                    div()
                        .id(ElementId::Name(format!("{id}-menu").into()))
                        .opacity(reveal)
                        .mt(px(-4.0 * (1.0 - reveal)))
                        .min_w(px(MENU_MIN_WIDTH))
                        .p(px(MENU_PADDING))
                        .flex()
                        .flex_col()
                        .rounded(px(MENU_RADIUS))
                        .bg(if dark {
                            palette.soft_fill
                        } else {
                            palette.field_surface
                        })
                        .border_1()
                        .border_color(lighting::rim(
                            if dark {
                                palette.soft_fill
                            } else {
                                palette.field_surface
                            },
                            dark,
                        ))
                        .shadow(lighting::panel(dark))
                        .track_focus(&focus)
                        .child(
                            // Prepaint is the first moment there is a
                            // `Window` to focus with, and the menu is only in
                            // the tree while it is open, so this runs exactly
                            // when the menu appears.
                            canvas(
                                move |_bounds, window, cx| {
                                    if focus_on_open.is_focused(window) {
                                        return;
                                    }
                                    // Remember where the keyboard was before
                                    // taking it, so closing can put it back.
                                    let previous = window.focused(cx);
                                    if let Some(host) = weak.upgrade() {
                                        host.update(cx, |host, _cx| {
                                            host.control_state_mut().menu_return_focus = previous;
                                        });
                                    }
                                    window.focus(&focus_on_open, cx);
                                },
                                |_bounds, _state, _window, _cx| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            let Some(key) = keyboard::key(event) else {
                                return;
                            };
                            cx.stop_propagation();
                            let at = this.control_state().menu_highlight();
                            match key {
                                Key::Dismiss => {
                                    this.control_state_mut().close_menu();
                                    restore_focus(this, window, cx);
                                }
                                Key::Activate => {
                                    if let Some(id) = at.and_then(|at| reachable_ids.get(at)) {
                                        let id = *id;
                                        this.control_state_mut().close_menu();
                                        restore_focus(this, window, cx);
                                        on_key_activate(this, id, window, cx);
                                    }
                                }
                                key => {
                                    // No row is lit until the first arrow, so a
                                    // menu opened with the mouse does not
                                    // pre-select something the pointer never
                                    // touched. Down starts at the top, up at the
                                    // bottom.
                                    let moved = match at {
                                        Some(at) => keyboard::step(
                                            key,
                                            Orientation::Vertical,
                                            at,
                                            reachable_ids.len(),
                                        ),
                                        None => match key {
                                            Key::Down | Key::Home => Some(0),
                                            Key::Up | Key::End => {
                                                reachable_ids.len().checked_sub(1)
                                            }
                                            _ => None,
                                        },
                                    };
                                    if let Some(moved) = moved {
                                        this.control_state_mut().highlight_menu(Some(moved));
                                    }
                                }
                            }
                            cx.notify();
                        }))
                        .occlude()
                        .on_mouse_down_out(cx.listener(
                            move |this, event: &MouseDownEvent, _window, cx| {
                                // A press on the button that opened this menu
                                // is a toggle, and the button's own click
                                // closes it on release. Closing here as well
                                // would close on the press and let the click
                                // open it straight back up.
                                let on_own_button = this
                                    .control_state()
                                    .track_bounds
                                    .get(id)
                                    .is_some_and(|bounds| bounds.contains(&event.position));
                                if on_own_button {
                                    return;
                                }
                                this.control_state_mut().close_menu();
                                cx.notify();
                            },
                        ))
                        // A right-click elsewhere should move the menu, not stack a
                        // second one; closing here lets the new press open it fresh.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|_this, _event: &MouseDownEvent, _window, _cx| {}),
                        )
                        .children(rows),
                ),
        )
        .with_priority(200),
    )
}

type Activate<V> = Rc<dyn Fn(&mut V, ComboId, &mut Window, &mut Context<V>)>;

/// Hands the keyboard back to whatever had it before the menu opened.
///
/// Without this, closing a menu leaves focus on an element that is no longer
/// in the tree, and the next Tab starts again from the top of the window —
/// which for someone navigating by keyboard means losing their place.
fn restore_focus<V: ControlHost>(view: &mut V, window: &mut Window, cx: &mut App) {
    if let Some(handle) = view.control_state_mut().menu_return_focus.take() {
        window.focus(&handle, cx);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_item<V: ControlHost>(
    index: usize,
    item: MenuItem,
    lit: f32,
    opened_at: Instant,
    palette: Palette,
    cx: &mut Context<V>,
    on_activate: &Activate<V>,
) -> gpui::AnyElement {
    match item {
        MenuItem::Separator => {
            let mut color: gpui::Hsla = crate::color::to_hsla(palette.field_border);
            color.alpha = 0.8;
            div()
                .my(px(3.0))
                .h(px(1.0))
                .w_full()
                .flex_none()
                .bg(color)
                .into_any_element()
        }
        MenuItem::Header(label) => div()
            .h(px(22.0))
            .flex_none()
            .px(px(8.0))
            .flex()
            .items_center()
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(palette.text_secondary)
            .whitespace_nowrap()
            .child(label)
            .into_any_element(),
        MenuItem::Action(action) => {
            let on_activate = on_activate.clone();
            let id = action.id;
            let label_color = if action.danger {
                palette.danger_label
            } else {
                palette.text_primary
            };
            let tick: gpui::Hsla = crate::color::to_hsla(label_color);
            div()
                .id(ElementId::NamedInteger(
                    format!("menu-item-{id}").into(),
                    index as u64,
                ))
                .h(px(ITEM_HEIGHT))
                .flex_none()
                .w_full()
                .pl(px(if action.checked { 6.0 } else { 8.0 }))
                .pr(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(MENU_RADIUS - 3.0))
                .text_size(px(12.5))
                .text_color(label_color)
                .whitespace_nowrap()
                .overflow_hidden()
                .when(!action.enabled, |el| el.opacity(0.45))
                // The keyboard's row is filled the same way the mouse fills
                // one under the pointer, so there is one idea of "this one"
                // rather than two competing marks.
                .when(lit > 0.01, move |el| {
                    el.bg(lighting::lit_at(
                        if action.danger {
                            palette.danger_fill
                        } else {
                            palette.control_fill
                        },
                        0.08,
                        lit,
                    ))
                })
                .when(action.enabled, move |el| {
                    let on_activate = on_activate.clone();
                    el.cursor_pointer()
                        .hover(move |style| {
                            style.bg(lighting::lit(
                                if action.danger {
                                    palette.danger_fill
                                } else {
                                    palette.control_fill
                                },
                                0.08,
                            ))
                        })
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            // The target the menu was summoned on lives in
                            // the same state the menu does, so closing first
                            // takes it away from the one callback that
                            // exists to receive it. Run, then close.
                            on_activate(this, id, window, cx);
                            // Unless the callback opened a menu of its own —
                            // a "Rename" that drops a second one is a fair
                            // thing for a host to do, and closing blind
                            // would swallow it.
                            if this.control_state().menu_opened_at() == Some(opened_at) {
                                this.control_state_mut().close_menu();
                            }
                            cx.notify();
                        }))
                })
                // Checked rows get a tick in a fixed gutter, so labels stay
                // aligned whether or not any row in the menu is checked.
                .when(action.checked, move |el| {
                    el.child(
                        div().w(px(12.0)).h(px(12.0)).flex_none().child(
                            canvas(
                                |_bounds, _window, _cx| {},
                                move |bounds, _state, window, _cx| {
                                    let o = bounds.origin;
                                    let mut builder = gpui::PathBuilder::stroke(px(1.5));
                                    builder.move_to(point(o.x + px(1.5), o.y + px(6.0)));
                                    builder.line_to(point(o.x + px(4.5), o.y + px(9.0)));
                                    builder.line_to(point(o.x + px(10.5), o.y + px(2.5)));
                                    if let Ok(path) = builder.build() {
                                        window.paint_path(path, tick);
                                    }
                                },
                            )
                            .size_full(),
                        ),
                    )
                })
                .child(div().flex_1().overflow_hidden().child(action.label))
                .children(action.shortcut.map(|shortcut| {
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(palette.text_secondary)
                        .child(shortcut)
                }))
                .into_any_element()
        }
    }
}

/// A button that drops a menu beneath itself instead of at the pointer.
///
/// The menu itself is still rendered by [`context_menu`] near the root; this
/// only opens it, anchored to the button's own bounds. Splitting it that way
/// keeps the list above everything, whatever the button is nested inside.
pub fn menu_button<V: ControlHost>(
    id: ComboId,
    label: &str,
    enabled: bool,
    palette: Palette,
    view: &V,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let dark = palette.is_dark;
    let open = view.control_state().is_menu_open(id);
    let fill = if dark {
        palette.soft_fill
    } else {
        palette.field_surface
    };
    let weak = cx.entity().downgrade();

    div()
        .id(ElementId::Name(format!("{id}-menu-button").into()))
        .relative()
        .h(px(crate::controls::CONTROL_HEIGHT))
        .flex_none()
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .rounded(px(crate::controls::CONTROL_RADIUS))
        .bg(lighting::lit(fill, 0.05))
        .border_1()
        .border_color(if open {
            palette.accent
        } else {
            lighting::rim(fill, dark)
        })
        .shadow(lighting::raised(dark))
        .text_size(px(12.5))
        .text_color(palette.text_primary)
        .whitespace_nowrap()
        .when(!enabled, |el| el.opacity(0.5))
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(move |style| style.bg(lighting::lit(fill, 0.09)))
        })
        .child(
            // Records the button's bounds so the menu can hang from them.
            canvas(
                move |bounds, _window, cx| {
                    if let Some(host) = weak.upgrade() {
                        host.update(cx, |host, _cx| {
                            host.control_state_mut().track_bounds.insert(id, bounds);
                        });
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(SharedString::from(label.to_string()))
        .child(
            div().w(px(9.0)).h(px(9.0)).flex_none().child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, _state, window, _cx| {
                        let o = bounds.origin;
                        let mut builder = gpui::PathBuilder::stroke(px(1.4));
                        builder.move_to(point(o.x + px(0.5), o.y + px(3.0)));
                        builder.line_to(point(o.x + px(4.5), o.y + px(7.0)));
                        builder.line_to(point(o.x + px(8.5), o.y + px(3.0)));
                        if let Ok(path) = builder.build() {
                            let color: gpui::Hsla = crate::color::to_hsla(palette.text_secondary);
                            window.paint_path(path, color);
                        }
                    },
                )
                .size_full(),
            ),
        )
        .when(enabled, |el| {
            el.on_click(cx.listener(move |this, _event, _window, cx| {
                let state = this.control_state_mut();
                if state.is_menu_open(id) {
                    state.close_menu();
                } else if let Some(bounds) = state.track_bounds.get(id).copied() {
                    state.open_menu_under(id, bounds, "");
                }
                cx.notify();
            }))
        })
}

/// The menu a context menu is currently showing, re-exported for hosts that
/// build their item list from it.
pub use crate::state::OpenMenu as ContextMenu;
