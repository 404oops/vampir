//! Right-click context menus.
//!
//! A menu is opened from wherever the press happened, by putting it in the
//! host's [`ControlState`](crate::ControlState), and rendered once near the root of the view.
//! Which items it holds is the host's decision each frame, because that
//! depends on what was clicked, which the state carries as an opaque target
//! string.
//!
//! ```ignore
//! // Around the row — a right-click opens the menu, aimed at this row:
//! vampir::menu_target("row", row_id, cx, |host, id, _cx| host.selected = Some(id), row)
//!
//! // Once, near the root of render:
//! .children(vampir::context_menu("row", &items, palette, self, cx,
//!     |host, item, _window, cx| host.run_menu_item(item, cx)))
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    Anchor, App, ClickEvent, ElementId, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, SharedString, Window, anchored, canvas, deferred, div, point, prelude::*, px,
};

use crate::controls::WidgetContext;
use crate::keyboard::{self, Dismiss, Key, Orientation};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{ComboId, ControlHost, ControlState, MenuBranch};

/// Metrics chosen so a menu of ordinary items lines up with the pop-up list
/// a [`crate::controls::combo`] drops, which is the same thing seen from a
/// different angle.
const ITEM_HEIGHT: f32 = 26.0;
const HEADER_HEIGHT: f32 = 22.0;
/// The space either side of a separator's hairline.
const SEPARATOR_GAP: f32 = 3.0;
const SEPARATOR_HEIGHT: f32 = 2.0 * SEPARATOR_GAP + 1.0;
const MENU_PADDING: f32 = 5.0;
const MENU_RADIUS: f32 = 7.0;
const MENU_MIN_WIDTH: f32 = 168.0;
const MENU_MAX_HEIGHT: f32 = 520.0;
/// Kept clear of the window edges, so a menu summoned in a corner still
/// reads as floating above the content rather than welded to the frame.
pub(crate) const VIEWPORT_MARGIN: f32 = 8.0;

/// One row of a context menu.
#[derive(Clone, Debug)]
pub enum MenuItem {
    Action(MenuAction),
    /// A branch whose rows replace this level in the same floating panel.
    Submenu {
        label: SharedString,
        items: Vec<MenuItem>,
        enabled: bool,
    },
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

    /// A nested level. Its leaf actions use the same activation callback as
    /// actions in the root menu.
    pub fn submenu(label: impl Into<SharedString>, items: Vec<MenuItem>) -> Self {
        MenuItem::Submenu {
            label: label.into(),
            items,
            enabled: true,
        }
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
        match &mut self {
            MenuItem::Action(action) => action.enabled = false,
            MenuItem::Submenu { enabled, .. } => *enabled = false,
            _ => {}
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

/// Identity is the branch's label and the shape of its own rows: the ids of
/// its actions and the labels of its branches. Checked state, disabled rows
/// and displayed action text can change while the menu is open without
/// turning it into a different branch. Nothing deeper is hashed: each level
/// below is checked by its own entry in the path, and folding it in here
/// would throw the reader out of a level that has not changed because of
/// an edit in a sibling's subtree.
fn submenu_identity(label: &SharedString, children: &[MenuItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    label.hash(&mut hasher);
    children.len().hash(&mut hasher);
    for item in children {
        match item {
            MenuItem::Action(action) => {
                0_u8.hash(&mut hasher);
                action.id.hash(&mut hasher);
            }
            MenuItem::Submenu { label, .. } => {
                1_u8.hash(&mut hasher);
                label.hash(&mut hasher);
            }
            MenuItem::Separator => 2_u8.hash(&mut hasher),
            MenuItem::Header(_) => 3_u8.hash(&mut hasher),
        }
    }
    hasher.finish()
}

/// The host supplies the whole tree each frame. A branch that vanished,
/// moved, changed identity or became disabled is no longer a valid place to
/// leave the keyboard.
fn visible_level<'a>(
    mut items: &'a [MenuItem],
    levels: &[MenuBranch],
) -> Option<(&'a [MenuItem], Option<&'a SharedString>)> {
    let mut title = None;
    for branch in levels {
        let MenuItem::Submenu {
            label,
            items: children,
            enabled: true,
        } = items.get(branch.index)?
        else {
            return None;
        };
        if submenu_identity(label, children) != branch.identity {
            return None;
        }
        items = children;
        title = Some(label);
    }
    Some((items, title))
}

/// Backs out of levels the host's latest tree no longer has, to the deepest
/// one it still does. Returns the branch it came back out of, if any.
///
/// The row that opened a vanished level may have moved or gone with it, so
/// the keyboard is put back on it only if a branch still stands at the same
/// place. Anywhere else the restored row could be an action the reader
/// never chose, one Enter away from running.
fn settle_levels(
    items: &[MenuItem],
    levels: &mut Vec<MenuBranch>,
    highlight: &mut Option<usize>,
) -> Option<MenuBranch> {
    let mut left = None;
    while visible_level(items, levels).is_none() {
        left = Some(levels.pop()?);
    }
    let branch = left?;
    let (level, _) = visible_level(items, levels)?;
    let still_there = reachable_indices(level).get(branch.highlight) == Some(&branch.index)
        && matches!(level.get(branch.index), Some(MenuItem::Submenu { .. }));
    *highlight = still_there.then_some(branch.highlight);
    Some(branch)
}

/// What a key does to the open menu, worked out without the window so it
/// can be tested.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MenuCommand {
    /// Up a level. At the root, Escape closes the menu and Left does
    /// nothing: Left means "back", and there is no back from the top.
    Back {
        close_at_root: bool,
    },
    Enter {
        row: usize,
        at: usize,
        identity: u64,
    },
    Run(ComboId),
    /// Light this reachable row.
    Highlight(usize),
}

fn menu_command(
    key: Key,
    at: Option<usize>,
    items: &[MenuItem],
    reachable: &[usize],
) -> Option<MenuCommand> {
    let at = at.filter(|at| *at < reachable.len());
    match key {
        Key::Dismiss => Some(MenuCommand::Back {
            close_at_root: true,
        }),
        Key::Left => Some(MenuCommand::Back {
            close_at_root: false,
        }),
        Key::Activate | Key::Right => {
            let at = at?;
            let row = *reachable.get(at)?;
            match items.get(row)? {
                MenuItem::Action(action) if key == Key::Activate => {
                    Some(MenuCommand::Run(action.id))
                }
                MenuItem::Submenu {
                    label,
                    items,
                    enabled: true,
                } => Some(MenuCommand::Enter {
                    row,
                    at,
                    identity: submenu_identity(label, items),
                }),
                _ => None,
            }
        }
        key => step_highlight(key, at, reachable.len()).map(MenuCommand::Highlight),
    }
}

/// No row is lit until the first arrow: Down starts at the top, Up at the
/// bottom. A level with nothing reachable has nowhere to go.
fn step_highlight(key: Key, at: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match at {
        Some(at) => keyboard::step(key, Orientation::Vertical, at, len),
        None => match key {
            Key::Down | Key::Home => Some(0),
            Key::Up | Key::End => Some(len - 1),
            _ => None,
        },
    }
}

fn reachable_indices(items: &[MenuItem]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            MenuItem::Action(action) if action.enabled => Some(index),
            MenuItem::Submenu { enabled: true, .. } => Some(index),
            _ => None,
        })
        .collect()
}

fn menu_height(items: &[MenuItem], nested: bool) -> f32 {
    let rows: f32 = items
        .iter()
        .map(|item| match item {
            MenuItem::Separator => SEPARATOR_HEIGHT,
            MenuItem::Header(_) => HEADER_HEIGHT,
            _ => ITEM_HEIGHT,
        })
        .sum();
    let back = if nested {
        ITEM_HEIGHT + SEPARATOR_HEIGHT
    } else {
        0.0
    };
    (rows + back + 2.0 * (MENU_PADDING + 1.0)).min(MENU_MAX_HEIGHT)
}

/// The panel's height in a window `window` tall. The window only moves a
/// menu back inside its edges; it never shrinks one, so a level taller than
/// the window has to be cut to fit here and scroll instead of overflowing.
fn fit_to_window(height: f32, window: Option<f32>) -> f32 {
    let Some(window) = window else {
        return height;
    };
    let room = window - 2.0 * VIEWPORT_MARGIN;
    height.min(room.max(ITEM_HEIGHT + 2.0 * (MENU_PADDING + 1.0)))
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
    // A host may replace the item tree while a menu is open. Walk back to
    // the last live branch rather than leaving the keyboard in a vanished
    // level with no rows to answer it.
    let state = view.control_state_mut();
    let left = {
        let menu = state.menu.as_mut()?;
        if menu.id != id {
            return None;
        }
        settle_levels(items, &mut menu.levels, &mut menu.highlight)
    };
    if let Some(branch) = left {
        state.menu_level_changed(branch.scroll);
    }
    let menu = state.menu.as_ref()?;
    let (anchor, under, opened_at, highlight, levels) = (
        menu.anchor,
        menu.under,
        menu.opened_at,
        menu.highlight,
        menu.levels.clone(),
    );
    let tree = items;
    let (level, title) = visible_level(items, &levels)?;
    let title = title.cloned();
    let items = level.to_vec();
    let key_items = items.clone();
    let reachable = reachable_indices(&items);
    let key_reachable = reachable.clone();
    let selected = highlight.filter(|index| *index < reachable.len());
    let nested = !levels.is_empty();
    let row_offset = if nested { 2 } else { 0 };
    let dark = palette.is_dark;
    let on_activate: Activate<V> = Rc::new(on_activate);
    let on_key_activate = on_activate.clone();
    let focus = view.control_state_mut().menu_focus(cx);
    let focus_on_open = focus.clone();
    let weak = cx.entity().downgrade();
    let scroll = view.control_state().menu_scroll(id);
    let key_scroll = scroll.clone();
    let window_height = view
        .control_state()
        .measured((id, "menu-window"))
        .map(|window| f32::from(window.size.height));

    // A menu that would run off the window is slid back inside it, the
    // margin clear of the edge, rather than cut off. `anchored` measures
    // the real panel to decide, so a long label is accounted for; this only
    // has to say which corner sits at the pointer.
    let at = under
        .map(|under| under.bottom_left() + point(px(0.0), px(4.0)))
        .unwrap_or(anchor);

    // The panel fades in over the same reveal a pop-up list uses, so the
    // two kinds of thing that drop out of a click arrive the same way.
    let reveal = crate::easing::ease_out_cubic(crate::easing::progress(
        opened_at,
        view.control_state().scaled(crate::state::COMBO_REVEAL),
    ));

    // Each level's rows get their own washes, so a row does not inherit the
    // light of the one that sat at its index in the level before.
    let level_key = {
        let mut hasher = DefaultHasher::new();
        for branch in &levels {
            (branch.index, branch.identity).hash(&mut hasher);
        }
        hasher.finish()
    };

    // A level change is noticed during render, regardless of whether it
    // came from a key, pointer, or host mutation. The panel keeps its
    // location while its rows and height settle into the next level.
    let depth = levels.len() as f32;
    let position =
        view.control_state()
            .tween((id, "menu-level", opened_at), depth, crate::state::MOVE);
    let level_reveal = (1.0 - (position - depth).abs()).clamp(0.0, 1.0);
    let level_offset = (depth - position) * 10.0;
    // The window's cap is applied outside the tween: it is a limit, not a
    // change of level, and a resize should not set the panel sliding.
    let height = fit_to_window(
        view.control_state().tween(
            (id, "menu-height", opened_at),
            menu_height(&items, nested),
            crate::state::MOVE,
        ),
        window_height,
    );

    let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(items.len() + row_offset);
    if let Some(title) = title {
        rows.push(render_back(title, id, opened_at, palette, cx));
        rows.push(render_item(
            0,
            None,
            MenuItem::Separator,
            0.0,
            id,
            opened_at,
            palette,
            cx,
            &on_activate,
        ));
    }
    for (index, item) in items.into_iter().enumerate() {
        let reachable_at = reachable.binary_search(&index).ok();
        let lit = selected.is_some_and(|at| reachable.get(at) == Some(&index));
        // The keyboard's wash moves from row to row rather than jumping.
        let lit = view.control_state().blend(
            (id, "menu-lit", (opened_at, level_key, index)),
            lit,
            crate::state::SWITCH_SLIDE,
        );
        rows.push(render_item(
            index,
            reachable_at,
            item,
            lit,
            id,
            opened_at,
            palette,
            cx,
            &on_activate,
        ));
    }

    let panel = div()
        .id(ElementId::Name(format!("{id}-menu").into()))
        .opacity(reveal)
        .mt(px(-4.0 * (1.0 - reveal)))
        .h(px(height))
        .min_w(px(MENU_MIN_WIDTH))
        .max_w(px(360.0))
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
            // Prepaint is the first moment there is a `Window` to focus
            // with, or to measure. The handle is unchanged as the menu
            // drills in or back.
            canvas(
                move |_bounds, window, cx| {
                    let viewport =
                        gpui::Bounds::new(point(px(0.0), px(0.0)), window.viewport_size());
                    if let Some(host) = weak.upgrade() {
                        let resized = host.update(cx, |host, _cx| {
                            let state = host.control_state_mut();
                            let resized = state.measured((id, "menu-window")) != Some(viewport);
                            if resized {
                                state.measure((id, "menu-window"), viewport);
                            }
                            resized
                        });
                        // Render has already used the old size. A notify
                        // from inside a draw is dropped, so ask for the
                        // next frame directly.
                        if resized {
                            window.request_animation_frame();
                        }
                    }
                    if focus_on_open.is_focused(window) {
                        return;
                    }
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
        // Once `bind_keys` has run, Escape arrives as this action and never
        // as a key. A child level consumes it to return to its parent.
        .on_action(cx.listener(move |this, _: &Dismiss, window, cx| {
            if !this.control_state_mut().back_menu() {
                this.control_state_mut().close_menu();
                restore_focus(this, window, cx);
            }
            cx.notify();
        }))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            let Some(key) = keyboard::key(event) else {
                return;
            };
            cx.stop_propagation();
            let at = this.control_state().menu_highlight();
            match menu_command(key, at, &key_items, &key_reachable) {
                Some(MenuCommand::Back { close_at_root }) => {
                    if !this.control_state_mut().back_menu() && close_at_root {
                        this.control_state_mut().close_menu();
                        restore_focus(this, window, cx);
                    }
                }
                Some(MenuCommand::Enter { row, at, identity }) => {
                    this.control_state_mut().enter_menu(row, at, identity, true);
                }
                Some(MenuCommand::Run(action)) => {
                    activate(this, action, opened_at, window, cx, &on_key_activate);
                }
                Some(MenuCommand::Highlight(moved)) => {
                    this.control_state_mut().highlight_menu(Some(moved));
                    if let Some(&row) = key_reachable.get(moved) {
                        key_scroll.scroll_to_item(row + row_offset);
                    }
                }
                None => {}
            }
            cx.notify();
        }))
        // Hover lights a row for the keyboard too, so the light has to go
        // when the pointer does: otherwise Enter runs a row the hand has
        // already left.
        .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
            if !*hovered && this.control_state().menu_highlight().is_some() {
                this.control_state_mut().highlight_menu(None);
                cx.notify();
            }
        }))
        .occlude()
        .on_mouse_down_out(
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                // The trigger toggles on release. Closing on its press would
                // let the same click immediately reopen the menu.
                let on_own_button = this
                    .control_state()
                    .track(id)
                    .is_some_and(|bounds| bounds.contains(&event.position));
                if on_own_button {
                    return;
                }
                this.control_state_mut().close_menu();
                restore_focus(this, window, cx);
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|_this, _event: &MouseDownEvent, _window, _cx| {}),
        )
        .child(
            // The slide moves the rows further than the padding is wide, so
            // the frame they slide in clips them: a lit row must not poke
            // past the border on its way in.
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .id(ElementId::Name(format!("{id}-menu-rows").into()))
                        .relative()
                        .left(px(level_offset))
                        .opacity(level_reveal)
                        .flex_1()
                        .min_h(px(0.0))
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&scroll)
                        .children(rows),
                ),
        )
        // The panel is as wide as the widest level in the tree, not the one
        // showing, so a level change never changes the width — nothing to
        // jump, and nothing for the edge snap to shove sideways. Tweening
        // the width instead would need last frame's measurement of the
        // content, and a frame drawn at the wrong width before the slide
        // could start. This way the layout engine works the width out from
        // the items in the same pass, and the control stays a function of
        // its data. The rows are laid out at no height, so they are never
        // seen and never hit.
        .child(
            div()
                .h(px(0.0))
                .flex_none()
                .flex()
                .flex_col()
                .overflow_hidden()
                .children(every_row(tree).into_iter().filter_map(sizing_row)),
        );
    // An occluding panel must keep an in-flight drag underneath it moving
    // and release it even if the pointer ends over the menu.
    let panel = crate::state::handle_mouse(panel, cx);
    Some(
        deferred(
            anchored()
                .position(at)
                .anchor(Anchor::TopLeft)
                .snap_to_window_with_margin(px(VIEWPORT_MARGIN))
                .child(panel),
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
    view.control_state_mut().restore_menu_focus(window, cx);
}

fn activate<V: ControlHost>(
    view: &mut V,
    id: ComboId,
    opened_at: Instant,
    window: &mut Window,
    cx: &mut Context<V>,
    on_activate: &Activate<V>,
) {
    // Restore first so a callback that opens another overlay remembers a
    // live control, while retaining the menu's target until it has run.
    restore_focus(view, window, cx);
    on_activate(view, id, window, cx);
    if view.control_state().menu_opened_at() == Some(opened_at) {
        view.control_state_mut().close_menu();
    }
}

/// Every row of every level, depth first: what the panel's width is taken
/// from.
fn every_row(items: &[MenuItem]) -> Vec<&MenuItem> {
    let mut rows = Vec::new();
    let mut stack = vec![items];
    while let Some(level) = stack.pop() {
        for item in level {
            rows.push(item);
            if let MenuItem::Submenu { items, .. } = item {
                stack.push(items);
            }
        }
    }
    rows
}

/// A row's content at the metrics `render_item` draws it with, and nothing
/// else. A branch is set in the back row's heavier weight, so it holds the
/// room for its title when the level it opens is showing.
fn sizing_row(item: &MenuItem) -> Option<gpui::AnyElement> {
    let row = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .pr(px(8.0))
        .whitespace_nowrap();
    Some(match item {
        MenuItem::Separator => return None,
        MenuItem::Header(label) => row
            .pl(px(8.0))
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .child(label.clone())
            .into_any_element(),
        MenuItem::Submenu { label, .. } => row
            .pl(px(8.0))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .child(label.clone())
            .child(div().w(px(10.0)).flex_none())
            .into_any_element(),
        MenuItem::Action(action) => row
            .pl(px(if action.checked { 6.0 } else { 8.0 }))
            .text_size(px(12.5))
            .when(action.checked, |row| {
                row.child(div().w(px(12.0)).flex_none())
            })
            .child(action.label.clone())
            .children(
                action
                    .shortcut
                    .clone()
                    .map(|shortcut| div().text_size(px(11.5)).child(shortcut)),
            )
            .into_any_element(),
    })
}

/// Whether a click on a row should count. A row click must belong to this
/// opening and to a level that has finished arriving; see
/// `ControlState::menu_takes_click`. A click counted
/// past one is the second half of a double-click whose first half changed
/// the level, so the row under it now was never aimed at either.
fn takes_click(state: &ControlState, opened_at: Instant, click_count: usize) -> bool {
    click_count <= 1 && state.menu_takes_click(opened_at)
}

/// The pointer lights the row it is over for the keyboard too. A disabled
/// row cannot hold the keyboard, and passing over one on the way to the
/// next should not drop the row that has it.
fn hover_row<V: ControlHost>(view: &mut V, reachable_at: Option<usize>, cx: &mut Context<V>) {
    if let Some(at) = reachable_at
        && view.control_state().menu_highlight() != Some(at)
    {
        view.control_state_mut().highlight_menu(Some(at));
        cx.notify();
    }
}

fn chevron(right: bool, palette: Palette) -> impl IntoElement {
    let color: gpui::Hsla = crate::color::to_hsla(palette.text_secondary);
    div().w(px(10.0)).h(px(10.0)).flex_none().child(
        canvas(
            |_bounds, _window, _cx| {},
            move |bounds, _state, window, _cx| {
                let origin = bounds.origin;
                let mut builder = gpui::PathBuilder::stroke(px(1.4));
                let edge = if right { 7.0 } else { 3.0 };
                let tip = if right { 3.0 } else { 7.0 };
                builder.move_to(point(origin.x + px(tip), origin.y + px(1.5)));
                builder.line_to(point(origin.x + px(edge), origin.y + px(5.0)));
                builder.line_to(point(origin.x + px(tip), origin.y + px(8.5)));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size_full(),
    )
}

fn render_back<V: ControlHost>(
    title: SharedString,
    menu_id: ComboId,
    opened_at: Instant,
    palette: Palette,
    cx: &mut Context<V>,
) -> gpui::AnyElement {
    div()
        .id(ElementId::Name(format!("{menu_id}-menu-back").into()))
        .h(px(ITEM_HEIGHT))
        .flex_none()
        .w_full()
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(MENU_RADIUS - 3.0))
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(palette.text_primary)
        .cursor_pointer()
        .hover(move |style| style.bg(lighting::lit(palette.control_fill, 0.08)))
        .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _window, cx| {
            if this.control_state().menu_highlight().is_some() {
                this.control_state_mut().highlight_menu(None);
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |this, event: &ClickEvent, _window, cx| {
            if takes_click(this.control_state(), opened_at, event.click_count()) {
                this.control_state_mut().back_menu();
                cx.notify();
            }
        }))
        .child(chevron(false, palette))
        .child(div().flex_1().overflow_hidden().child(title))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_item<V: ControlHost>(
    index: usize,
    reachable_at: Option<usize>,
    item: MenuItem,
    lit: f32,
    menu_id: ComboId,
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
                .my(px(SEPARATOR_GAP))
                .h(px(1.0))
                .w_full()
                .flex_none()
                .bg(color)
                .into_any_element()
        }
        MenuItem::Header(label) => div()
            .h(px(HEADER_HEIGHT))
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
        MenuItem::Submenu {
            label,
            items,
            enabled,
        } => {
            let identity = submenu_identity(&label, &items);
            div()
                .id(ElementId::NamedInteger(
                    format!("{menu_id}-submenu").into(),
                    index as u64,
                ))
                .h(px(ITEM_HEIGHT))
                .flex_none()
                .w_full()
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(MENU_RADIUS - 3.0))
                .text_size(px(12.5))
                .text_color(palette.text_primary)
                .whitespace_nowrap()
                .overflow_hidden()
                .when(!enabled, |el| el.opacity(0.45))
                .when(lit > 0.01, |el| {
                    el.bg(lighting::lit_at(palette.control_fill, 0.08, lit))
                })
                .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _window, cx| {
                    hover_row(this, reachable_at, cx);
                }))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .hover(move |style| style.bg(lighting::lit(palette.control_fill, 0.08)))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _window, cx| {
                            if takes_click(this.control_state(), opened_at, event.click_count())
                                && let Some(at) = reachable_at
                            {
                                this.control_state_mut()
                                    .enter_menu(index, at, identity, false);
                                cx.notify();
                            }
                        }))
                })
                .child(div().flex_1().overflow_hidden().child(label))
                .child(chevron(true, palette))
                .into_any_element()
        }
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
                .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _window, cx| {
                    hover_row(this, reachable_at, cx);
                }))
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
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if takes_click(this.control_state(), opened_at, event.click_count()) {
                                activate(this, id, opened_at, window, cx, &on_activate);
                                cx.notify();
                            }
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
                                    let origin = bounds.origin;
                                    let mut builder = gpui::PathBuilder::stroke(px(1.5));
                                    builder.move_to(point(origin.x + px(1.5), origin.y + px(6.0)));
                                    builder.line_to(point(origin.x + px(4.5), origin.y + px(9.0)));
                                    builder.line_to(point(origin.x + px(10.5), origin.y + px(2.5)));
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
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
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
                            host.control_state_mut().record_track(id, bounds);
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
                        let origin = bounds.origin;
                        let mut builder = gpui::PathBuilder::stroke(px(1.4));
                        builder.move_to(point(origin.x + px(0.5), origin.y + px(3.0)));
                        builder.line_to(point(origin.x + px(4.5), origin.y + px(7.0)));
                        builder.line_to(point(origin.x + px(8.5), origin.y + px(3.0)));
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
            el.on_click(cx.listener(move |this, _event, window, cx| {
                let state = this.control_state_mut();
                if state.is_menu_open(id) {
                    state.close_menu();
                    state.restore_menu_focus(window, cx);
                } else if let Some(bounds) = state.track(id) {
                    state.open_menu_under(id, bounds, "");
                }
                cx.notify();
            }))
        })
        .when(enabled, |el| {
            el.child(
                keyboard::ring(
                    ElementId::Name(format!("{id}-menu-button-focus").into()),
                    crate::controls::CONTROL_RADIUS,
                    palette,
                )
                .on_key_down(cx.listener(
                    move |this, event: &KeyDownEvent, _window, cx| {
                        if !matches!(keyboard::key(event), Some(Key::Activate | Key::Down)) {
                            return;
                        }
                        cx.stop_propagation();
                        if let Some(bounds) = this.control_state().track(id) {
                            this.control_state_mut().open_menu_under(id, bounds, "");
                            cx.notify();
                        }
                    },
                )),
            )
        })
}

/// Wraps `child` so that a right-click on it opens the context menu `menu`,
/// aimed at `target`, after running `on_open` with the same target.
///
/// The listener belongs on the row rather than on the list, because the
/// menu has to be about what was actually clicked: hung on the container
/// instead, it only ever knows about the selection, and right-clicking one
/// row opens a menu aimed at another. `on_open` is where a host selects the
/// row on the way, which is what every file list does and what keeps the
/// highlight and the menu saying the same thing.
///
/// ```ignore
/// menu_target("row", file.id.clone(), cx, |this, id, _cx| this.selected = Some(id),
///     tree_row(..))
/// ```
pub fn menu_target<V: ControlHost>(
    menu: ComboId,
    target: impl Into<SharedString>,
    cx: &mut Context<V>,
    on_open: impl Fn(&mut V, SharedString, &mut Context<V>) + 'static,
    child: impl IntoElement,
) -> gpui::Div {
    let target = target.into();
    div()
        .flex_none()
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                on_open(this, target.clone(), cx);
                this.control_state_mut()
                    .open_menu(menu, event.position, target.clone());
                cx.notify();
            }),
        )
        .child(child)
}

/// The menu a context menu is currently showing, re-exported for hosts that
/// build their item list from it.
pub use crate::state::OpenMenu as ContextMenu;

#[cfg(test)]
mod tests {
    use super::{
        MenuBranch, MenuCommand, MenuItem, every_row, fit_to_window, menu_command, menu_height,
        reachable_indices, settle_levels, step_highlight, submenu_identity, takes_click,
        visible_level,
    };
    use crate::ControlState;
    use crate::keyboard::Key;

    fn branch(item: &MenuItem, index: usize, highlight: usize) -> MenuBranch {
        let MenuItem::Submenu { label, items, .. } = item else {
            panic!("expected submenu");
        };
        MenuBranch {
            index,
            highlight,
            identity: submenu_identity(label, items),
            scroll: Default::default(),
        }
    }

    fn children(item: &MenuItem) -> &[MenuItem] {
        let MenuItem::Submenu { items, .. } = item else {
            panic!("expected submenu");
        };
        items
    }

    /// File > Organize > Move to, with a sibling branch beside Move to.
    fn tree(move_to: &[&'static str], tag: &[&'static str]) -> Vec<MenuItem> {
        let actions = |ids: &[&'static str]| {
            ids.iter()
                .map(|id| MenuItem::action(id, *id))
                .collect::<Vec<_>>()
        };
        vec![
            MenuItem::action("open", "Open"),
            MenuItem::submenu(
                "Organize",
                vec![
                    MenuItem::action("pin", "Pin"),
                    MenuItem::submenu("Move to", actions(move_to)),
                    MenuItem::submenu("Tag", actions(tag)),
                ],
            ),
        ]
    }

    fn path_to_move(items: &[MenuItem]) -> Vec<MenuBranch> {
        vec![
            branch(&items[1], 1, 1),
            branch(&children(&items[1])[1], 1, 1),
        ]
    }

    #[test]
    fn nested_levels_find_the_leaf_and_reject_a_removed_branch() {
        let items = vec![MenuItem::submenu(
            "Organize",
            vec![MenuItem::submenu(
                "Move to",
                vec![MenuItem::action("inbox", "Inbox")],
            )],
        )];
        let MenuItem::Submenu {
            items: children, ..
        } = &items[0]
        else {
            unreachable!();
        };
        let path = [branch(&items[0], 0, 0), branch(&children[0], 0, 0)];
        let (level, title) = visible_level(&items, &path).expect("nested level");
        assert_eq!(title.map(|title| title.as_ref()), Some("Move to"));
        assert!(matches!(level, [MenuItem::Action(action)] if action.id == "inbox"));

        let disabled = vec![MenuItem::submenu("Organize", vec![]).disabled()];
        assert!(visible_level(&disabled, &path[..1]).is_none());
    }

    #[test]
    fn a_reordered_or_replaced_branch_cannot_show_another_submenu() {
        let original = vec![
            MenuItem::submenu("Organize", vec![MenuItem::action("inbox", "Inbox")]),
            MenuItem::submenu("Other", vec![MenuItem::action("settings", "Settings")]),
        ];
        let path = [branch(&original[0], 0, 0)];
        assert!(visible_level(&original, &path).is_some());

        let reordered = vec![original[1].clone(), original[0].clone()];
        assert!(visible_level(&reordered, &path).is_none());
        let replaced = vec![MenuItem::submenu(
            "Organize",
            vec![MenuItem::action("archive", "Archive")],
        )];
        assert!(visible_level(&replaced, &path).is_none());

        let relabelled = vec![MenuItem::submenu(
            "Organize",
            vec![MenuItem::action("inbox", "Move to Inbox")],
        )];
        assert!(visible_level(&relabelled, &path).is_some());
    }

    /// Editing one branch's subtree must not throw the reader out of
    /// another: only the levels on the path, and their own rows, count.
    #[test]
    fn an_edit_in_a_sibling_subtree_keeps_the_open_level() {
        let original = tree(&["inbox"], &["red"]);
        let path = path_to_move(&original);
        let retagged = tree(&["inbox"], &["red", "blue"]);
        let (level, title) = visible_level(&retagged, &path).expect("still open");
        assert_eq!(title.map(|title| title.as_ref()), Some("Move to"));
        assert!(matches!(level, [MenuItem::Action(action)] if action.id == "inbox"));

        // The open level's own rows changing still rejects it.
        let moved = tree(&["archive"], &["red"]);
        assert!(visible_level(&moved, &path).is_none());
        assert!(visible_level(&moved, &path[..1]).is_some());
    }

    #[test]
    fn settling_backs_out_only_as_far_as_the_tree_changed() {
        let original = tree(&["inbox"], &["red"]);
        let path = path_to_move(&original);

        let mut levels = path.clone();
        let mut highlight = Some(0);
        let retagged = tree(&["inbox"], &["red", "blue"]);
        assert_eq!(settle_levels(&retagged, &mut levels, &mut highlight), None);
        assert_eq!((levels.len(), highlight), (2, Some(0)));

        // Move to's rows changed: back to Organize, keyboard on Move to,
        // which is still a branch in the same place.
        let moved = tree(&["archive"], &["red"]);
        let left = settle_levels(&moved, &mut levels, &mut highlight);
        assert_eq!(left, Some(path[1]));
        assert_eq!((levels.len(), highlight), (1, Some(1)));

        // Organize itself gone: back to the root, with nothing lit rather
        // than a stranger in Organize's place.
        let mut levels = path.clone();
        let mut highlight = Some(0);
        let flattened = vec![
            MenuItem::action("open", "Open"),
            MenuItem::action("delete", "Delete").danger(),
        ];
        let left = settle_levels(&flattened, &mut levels, &mut highlight);
        assert_eq!(left, Some(path[0]));
        assert!(levels.is_empty());
        assert_eq!(highlight, None);
    }

    #[test]
    fn arrows_in_a_level_with_nothing_reachable_go_nowhere() {
        for key in [Key::Down, Key::Home, Key::Up, Key::End] {
            assert_eq!(step_highlight(key, None, 0), None);
            assert_eq!(step_highlight(key, Some(3), 0), None);
        }
        let inert = vec![
            MenuItem::header("Recent"),
            MenuItem::separator(),
            MenuItem::action("none", "Nothing yet").disabled(),
        ];
        let reachable = reachable_indices(&inert);
        assert!(reachable.is_empty());
        assert_eq!(menu_command(Key::Down, None, &inert, &reachable), None);
        assert_eq!(menu_command(Key::Home, Some(0), &inert, &reachable), None);
        assert_eq!(
            menu_command(Key::Activate, Some(0), &inert, &reachable),
            None
        );

        assert_eq!(step_highlight(Key::Down, None, 3), Some(0));
        assert_eq!(step_highlight(Key::Up, None, 3), Some(2));
    }

    /// Left is "back" and Escape is "back, or away": at the root only
    /// Escape closes the menu.
    #[test]
    fn left_and_escape_go_back_and_only_escape_closes_the_root() {
        let items = tree(&["inbox"], &["red"]);
        let reachable = reachable_indices(&items);
        assert_eq!(
            menu_command(Key::Left, None, &items, &reachable),
            Some(MenuCommand::Back {
                close_at_root: false
            })
        );
        assert_eq!(
            menu_command(Key::Dismiss, None, &items, &reachable),
            Some(MenuCommand::Back {
                close_at_root: true
            })
        );
        assert_eq!(
            menu_command(Key::Right, Some(1), &items, &reachable),
            Some(MenuCommand::Enter {
                row: 1,
                at: 1,
                identity: branch(&items[1], 1, 1).identity,
            })
        );
        assert_eq!(
            menu_command(Key::Activate, Some(0), &items, &reachable),
            Some(MenuCommand::Run("open"))
        );
        assert_eq!(menu_command(Key::Right, Some(0), &items, &reachable), None);
    }

    #[test]
    fn navigation_skips_inert_rows_but_enters_enabled_submenus() {
        let items = vec![
            MenuItem::header("Files"),
            MenuItem::action("open", "Open"),
            MenuItem::separator(),
            MenuItem::action("closed", "Closed").disabled(),
            MenuItem::submenu("Organize", vec![MenuItem::action("move", "Move")]),
            MenuItem::submenu("Unavailable", vec![]).disabled(),
        ];
        assert_eq!(reachable_indices(&items), vec![1, 4]);
    }

    #[test]
    fn long_levels_scroll_and_nested_levels_make_room_for_back() {
        let short = vec![MenuItem::action("open", "Open")];
        assert_eq!(menu_height(&short, true) - menu_height(&short, false), 33.0);
        let long: Vec<_> = (0..40).map(|_| MenuItem::header("Section")).collect();
        assert_eq!(menu_height(&long, false), super::MENU_MAX_HEIGHT);
    }

    /// The second click of a double-click is refused however late it comes,
    /// and any click is refused while a level is still arriving.
    #[test]
    fn row_clicks_need_a_single_click_on_a_settled_level() {
        let mut state = ControlState::new();
        state.open_menu("row", gpui::point(gpui::px(0.0), gpui::px(0.0)), "");
        let opening = state.menu_opened_at().expect("open");
        assert!(takes_click(&state, opening, 1));
        assert!(!takes_click(&state, opening, 2));

        state.enter_menu(1, 1, 7, false);
        assert!(!takes_click(&state, opening, 1));
        state.set_time_scale(0.001);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(takes_click(&state, opening, 1));
        assert!(!takes_click(&state, opening, 2));
    }

    /// The panel's height is worked out from the same constants the rows
    /// are drawn at, so the two cannot drift apart.
    #[test]
    fn menu_height_counts_rows_at_their_drawn_heights() {
        let chrome = 2.0 * (super::MENU_PADDING + 1.0);
        let one = |item: MenuItem| menu_height(&[item], false) - chrome;
        assert_eq!(one(MenuItem::separator()), 7.0);
        assert_eq!(one(MenuItem::header("Recent")), 22.0);
        assert_eq!(one(MenuItem::action("open", "Open")), super::ITEM_HEIGHT);
        assert_eq!(super::SEPARATOR_HEIGHT, 2.0 * super::SEPARATOR_GAP + 1.0);
    }

    /// The panel takes its width from every level, so a row deep in a
    /// branch the reader has not opened still counts.
    #[test]
    fn the_width_is_taken_from_every_level() {
        let items = tree(&["inbox"], &["a-very-long-tag"]);
        let ids: Vec<_> = every_row(&items)
            .into_iter()
            .filter_map(|item| match item {
                MenuItem::Action(action) => Some(action.id),
                _ => None,
            })
            .collect();
        for id in ["open", "pin", "inbox", "a-very-long-tag"] {
            assert!(ids.contains(&id), "{id} missing");
        }
    }

    #[test]
    fn a_short_window_cuts_the_panel_to_fit() {
        assert_eq!(fit_to_window(520.0, None), 520.0);
        assert_eq!(fit_to_window(520.0, Some(900.0)), 520.0);
        assert_eq!(
            fit_to_window(520.0, Some(300.0)),
            300.0 - 2.0 * super::VIEWPORT_MARGIN
        );
        // A window too small for anything still leaves one row's worth.
        assert_eq!(fit_to_window(520.0, Some(10.0)), 38.0);
    }
}
