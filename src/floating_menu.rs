//! A context menu in a transparent pop-up window of its own, for a host
//! whose GPUI window has a live native view over it.
//!
//! It behaves like a native menu: it floats above the window, one is open
//! at a time, and a click anywhere else, a key it has no use for, or
//! switching to another window closes it. Submenus open in place with a
//! row at the top that leads back. The arrows walk the rows, Enter runs
//! one, Right and Left go into a submenu and out again, and typing a
//! label's first letters lands on it.
//!
//! The rows are the same [`MenuItem`] tree the in-window
//! [`context_menu`](crate::context_menu) takes, so a host builds a menu
//! once and shows it whichever way the window allows.
//!
//! ```ignore
//! floating_menu::open_with(cx, window, event.position, items, palette,
//!     |choice, window, cx| {
//!         if let Some(command) = choice {
//!             let _ = host.update(cx, |host, cx| host.run(command, window, cx));
//!         }
//!     });
//! ```
//!
//! The answer is the chosen action's id; `None` when the menu was
//! dismissed. The window is a [`gpui::WindowKind::PopUp`], which every
//! platform has. It takes the keyboard while it is open; on macOS with the
//! `native` feature it instead watches the app's events, so the field or
//! page being typed in keeps its caret and the click that closes the menu
//! goes no further, as with a native menu.

use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, AnyElement, AnyWindowHandle, App, Bounds, Context, DisplayId,
    FontWeight, KeyDownEvent, Point, Render, Rgba, ScrollHandle, SharedString, Subscription,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, canvas, div,
    point, prelude::*, px, size,
};

use crate::{
    color,
    easing::slowed,
    keyboard::{self, Key, Orientation},
    lighting,
    menu::{MenuIcon, MenuItem},
    palette::Palette,
    state::{ControlHost, ControlState},
    typography::ui_font,
};

#[cfg(all(target_os = "macos", feature = "native"))]
use crate::native::menu_watch as platform;

/// What the platform does for a menu beyond what GPUI does everywhere:
/// nothing, unless the `native` feature on macOS says otherwise.
#[cfg(not(all(target_os = "macos", feature = "native")))]
mod platform {
    use gpui::{App, Window, WindowHandle};

    /// Whether the menu's window keeps the keyboard away from the host's.
    pub const KEYLESS: bool = false;
    pub struct Watch;
    pub fn watch<I: Clone + 'static>(
        _window: &Window,
        _menu: WindowHandle<super::Menu<I>>,
        _cx: &App,
    ) -> Option<Watch> {
        None
    }
    /// Shows the window, opened hidden: its first draw has settled the
    /// transparent gutter around the panel before the platform sees it,
    /// so no window shadow is cast from the whole popup's outline.
    pub fn shown(window: &mut Window, cx: &mut App) {
        window.draw(cx).clear(cx);
        window.activate_window();
    }
    pub fn hide(_window: &Window) {}
}

/// A menu is as wide as its longest row, within these.
const MIN_WIDTH: f32 = 184.0;
const MAX_WIDTH: f32 = 340.0;
const ROW_HEIGHT: f32 = 28.0;
const HEADER_HEIGHT: f32 = 22.0;
/// A separator's hairline and the room either side of it.
const SEPARATOR_HEIGHT: f32 = 11.0;
/// Between the panel's edge and its rows.
const PADDING: f32 = 6.0;
const BORDER: f32 = 1.0;
const RADIUS: f32 = 12.0;
const ROW_RADIUS: f32 = 7.0;
/// The shared panel shadow reaches three blur radii (30 points), plus
/// its two-point downward offset. Keep its tail inside the popup, where
/// the popup may have clear room around its panel at all.
const EDGE_GUTTER: f32 = if crate::popup::CLEAR_MARGINS {
    32.0
} else {
    0.0
};
/// Between a row's highlight and what's in it.
const ROW_INSET: f32 = 8.0;
const TEXT_SIZE: f32 = 13.0;
const SHORTCUT_SIZE: f32 = 11.5;
const ICON_SIZE: f32 = 15.0;
/// The column icons sit in, and the one checks and chevrons sit in.
const ICON_SLOT: f32 = 18.0;
const TRAILING_SLOT: f32 = 14.0;
const GAP: f32 = 8.0;
/// Taller menus scroll.
const MAX_HEIGHT: f32 = 520.0;
/// Clear space the popup keeps above its panel. AppKit treats the top of
/// any titled window, even one with no visible title bar, as its title
/// bar: a click there in a window that isn't key goes to moving the
/// window instead of to the view. Only AppKit has the band, and only a
/// window with clear margins can keep it empty.
const TITLE_BAND: f32 = if crate::popup::CLEAR_MARGINS {
    28.0
} else {
    0.0
};
/// The back row's key among a level's rows, which are keyed by index.
const BACK_ROW: usize = usize::MAX;

/// How long the panel opens, its rows sweep, and its size changes.
const SWEEP: Duration = Duration::from_millis(190);
const ROW_SWEEP: Duration = Duration::from_millis(170);
const RESIZE: Duration = Duration::from_millis(170);
const HOVER: Duration = Duration::from_millis(120);
/// Letters typed this close together are one search for a row.
const TYPE_AHEAD_RESET: Duration = Duration::from_millis(1000);

// ---- Geometry ---------------------------------------------------------------

/// Which way a menu comes in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sweep {
    /// Unrolling down from the pointer.
    Down,
    /// Unrolling up from a shelf.
    Up,
    /// Into a submenu: rows arrive from the right.
    Forward,
    /// Back out of one: rows arrive from the left.
    Back,
}

impl Sweep {
    /// Where a row starts, relative to where it settles.
    fn offset(self) -> (f32, f32) {
        match self {
            Sweep::Down => (0.0, -6.0),
            Sweep::Up => (0.0, 6.0),
            Sweep::Forward => (16.0, 0.0),
            Sweep::Back => (-16.0, 0.0),
        }
    }

    /// Whether the panel itself unrolls, rather than just its rows.
    fn unrolls(self) -> bool {
        matches!(self, Sweep::Down | Sweep::Up)
    }
}

/// One level of a menu: the top, or a submenu opened from it.
#[derive(Clone)]
struct Level<I> {
    items: Vec<MenuItem<I>>,
    /// The row that opened it, shown at the top as the way back.
    title: Option<SharedString>,
}

/// The rows of `level` the keyboard can land on, in order.
fn reachable_rows<I>(level: &Level<I>) -> Vec<usize> {
    level
        .items
        .iter()
        .enumerate()
        .filter_map(|(row, item)| item.reachable().then_some(row))
        .collect()
}

/// The height `level` needs, with its way back if it's nested.
fn height_of<I>(level: &Level<I>) -> f32 {
    height_of_items(&level.items, level.title.is_some())
}

fn height_of_items<I>(items: &[MenuItem<I>], has_title: bool) -> f32 {
    let rows: f32 = items
        .iter()
        .map(|item| match item {
            MenuItem::Separator => SEPARATOR_HEIGHT,
            MenuItem::Header(_) => HEADER_HEIGHT,
            _ => ROW_HEIGHT,
        })
        .sum();
    let back = if has_title {
        ROW_HEIGHT + SEPARATOR_HEIGHT
    } else {
        0.0
    };
    (rows + back + 2.0 * (PADDING + BORDER)).min(MAX_HEIGHT)
}

/// Whether any row of `items` has an icon, so every label lines up after
/// the icon column; and whether any has a check or a chevron after it.
fn columns<I>(items: &[MenuItem<I>]) -> (bool, bool) {
    let icons = MenuItem::any_icon(items);
    let trailing = items.iter().any(|item| match item {
        MenuItem::Submenu { .. } => true,
        MenuItem::Action(action) => action.checked,
        _ => false,
    });
    (icons, trailing)
}

/// The width that fits `level`'s longest row, given how wide `measure`
/// finds a label at a weight.
fn width_of_level<I>(level: &Level<I>, measure: impl Fn(&str, FontWeight) -> f32) -> f32 {
    width_of_items(&level.items, level.title.as_deref(), measure)
}

fn width_of_items<I>(
    items: &[MenuItem<I>],
    title: Option<&str>,
    measure: impl Fn(&str, FontWeight) -> f32,
) -> f32 {
    let (icons, trailing) = columns(items);
    let label = items
        .iter()
        .filter_map(|item| {
            let label = item.label()?;
            Some(match item {
                MenuItem::Header(_) => measure(label, FontWeight::MEDIUM),
                MenuItem::Action(action) => {
                    let shortcut = action
                        .shortcut
                        .as_deref()
                        .map_or(0.0, |shortcut| measure(shortcut, FontWeight::NORMAL) + GAP);
                    measure(label, FontWeight::NORMAL) + shortcut
                }
                _ => measure(label, FontWeight::NORMAL),
            })
        })
        .fold(0.0, f32::max);
    let mut row = label;
    if icons {
        row += ICON_SLOT + GAP;
    }
    if trailing {
        row += TRAILING_SLOT + GAP;
    }
    // The way back keeps the level's columns, with its chevron in the
    // icon's and its title set medium.
    let back = title.map_or(0.0, |title| {
        let mut back = ICON_SLOT + GAP + measure(title, FontWeight::MEDIUM);
        if trailing {
            back += TRAILING_SLOT + GAP;
        }
        back
    });
    // A little over, so the longest label isn't flush against the edge.
    let slack = 12.0;
    (row.max(back) + 2.0 * (PADDING + BORDER + ROW_INSET) + slack).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// How wide `text` sets in the menu's font at `weight`.
fn measure(cx: &App, text: &str, weight: FontWeight) -> f32 {
    crate::typography::text_width(cx, text, TEXT_SIZE, weight)
}

/// A transparent popup large enough for any level, so changing levels does
/// not resize its native window.
fn level_sizes<I>(
    level: &Level<I>,
    measure: impl Fn(&str, FontWeight) -> f32 + Copy,
) -> ((f32, f32), (f32, f32)) {
    let root = (width_of_level(level, measure), height_of(level));
    (root, maximum_size_from(&level.items, root, measure))
}

fn maximum_size_from<I>(
    items: &[MenuItem<I>],
    (mut width, mut height): (f32, f32),
    measure: impl Fn(&str, FontWeight) -> f32 + Copy,
) -> (f32, f32) {
    for item in items {
        if let MenuItem::Submenu { label, items, .. } = item {
            let child = (
                width_of_items(items, Some(label), measure),
                height_of_items(items, true),
            );
            let (child_width, child_height) = maximum_size_from(items, child, measure);
            width = width.max(child_width);
            height = height.max(child_height);
        }
    }
    (width, height)
}

/// Where the popup goes for a menu summoned at `pointer`: its panel's left
/// and opening edge at the pointer, the transparent gutter around it, and
/// the whole of it on `screen`. A menu that would run off the bottom opens
/// upward from the pointer instead, as a native one does, when there is
/// room above; only when there is room neither way is it slid back inside
/// the edge. Returns the origin and whether it opens upward.
fn place(
    pointer: Point<f32>,
    (width, height): (f32, f32),
    screen: Option<Bounds<f32>>,
    above: bool,
) -> (Point<f32>, bool) {
    let origin_for = |above: bool| {
        point(
            pointer.x - EDGE_GUTTER,
            pointer.y
                - if above {
                    height - EDGE_GUTTER
                } else {
                    TITLE_BAND
                },
        )
    };
    let mut above = above;
    let mut origin = origin_for(above);
    if let Some(screen) = screen {
        let overflows = origin.y + height > screen.bottom();
        let fits_above = origin_for(true).y >= screen.top();
        if !above && overflows && fits_above {
            above = true;
            origin = origin_for(true);
        }
        origin.x = origin.x.min(screen.right() - width).max(screen.left());
        origin.y = origin.y.min(screen.bottom() - height).max(screen.top());
    }
    (origin, above)
}

// ---- The keyboard -----------------------------------------------------------

/// What a key press means to the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    /// One of the keys the menu walks itself with.
    Key(Key),
    /// A letter towards a row's label.
    Text(String),
    /// Anything else: a shortcut, Tab, a modifier chord. The menu closes
    /// and lets it through.
    Other,
}

/// Sorts a key press by the key it names and the text it carries. A press
/// with a platform or control modifier is never the menu's: it is a
/// shortcut the host may have, and a menu that swallowed ⌘Q would be a
/// menu nobody could get past.
pub(crate) fn typed(key: Option<Key>, text: Option<&str>, chord: bool) -> Typed {
    if chord {
        return Typed::Other;
    }
    if let Some(key) = key {
        return Typed::Key(key);
    }
    match text {
        Some(text)
            if !text.is_empty()
                && text
                    .chars()
                    .all(|ch| !ch.is_control() && !ch.is_whitespace()) =>
        {
            Typed::Text(text.to_string())
        }
        _ => Typed::Other,
    }
}

/// [`typed`] for a key GPUI delivered to the menu's own window.
fn typed_event(event: &KeyDownEvent) -> Typed {
    let modifiers = &event.keystroke.modifiers;
    let key = keyboard::key(event).filter(|key| {
        matches!(
            key,
            Key::Dismiss
                | Key::Activate
                | Key::Up
                | Key::Down
                | Key::Left
                | Key::Right
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
        )
    });
    typed(
        key,
        event.keystroke.key_char.as_deref(),
        modifiers.platform || modifiers.control,
    )
}

/// The row `typed` lands on, among `labels` in order, from `current`.
/// Letters typed in a row match a label's start; the same letter typed
/// again cycles through the rows that start with it, as a native menu
/// does. Nothing matched is nothing moved.
fn type_ahead(labels: &[&str], typed: &str, current: Option<usize>) -> Option<usize> {
    if labels.is_empty() || typed.is_empty() {
        return None;
    }
    let lower = typed.to_lowercase();
    let starts = |label: &str, prefix: &str| label.to_lowercase().starts_with(prefix);
    let mut letters = lower.chars();
    let first = letters.next()?;
    let repeated = letters.all(|ch| ch == first) && lower.chars().count() > 1;
    if repeated {
        let first = first.to_string();
        let from = current.map_or(0, |current| current + 1);
        return (0..labels.len())
            .map(|offset| (from + offset) % labels.len())
            .find(|&row| starts(labels[row], &first));
    }
    labels.iter().position(|label| starts(label, &lower))
}

/// The next reachable row for an arrow, from `at` among `len` of them.
/// Nothing lit yet: Down starts at the top, Up at the bottom.
fn step_row(key: Key, at: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match at {
        Some(at) => keyboard::step(key, Orientation::Vertical, at, len),
        None => match key {
            Key::Down | Key::Home | Key::PageDown => Some(0),
            Key::Up | Key::End | Key::PageUp => Some(len - 1),
            _ => None,
        },
    }
}

// ---- The window -------------------------------------------------------------

/// What reaches a menu from outside its rows: the platform's watch, or
/// GPUI's own key dispatch.
pub(crate) enum Signal {
    Close,
    Key(Key),
    Text(String),
}

/// The menu showing, and how to close it as dismissed from outside.
struct Open {
    window: AnyWindowHandle,
    dismiss: Box<dyn FnOnce(&mut App)>,
}

thread_local! {
    /// The menu showing, if one is, and how to close it.
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
}

pub(crate) struct Menu<I> {
    controls: ControlState,
    palette: Palette,
    levels: Vec<Level<I>>,
    /// The rows of the current level the keyboard can land on.
    reachable: Vec<usize>,
    answer: Option<Answer<I>>,
    watch: Option<platform::Watch>,
    _activation: Option<Subscription>,
    /// Whether the window has been the active one: only then does losing
    /// that mean the reader went elsewhere.
    was_active: bool,
    done: bool,
    /// The first draw warms text and icons; the short opening animation
    /// starts after it.
    animations_ready: bool,
    sweep: Sweep,
    anchored_above: bool,
    animation_epoch: usize,
    /// The visible panel's target size. The popup itself fits every level.
    width: f32,
    height: f32,
    /// The size every level's panel takes, where the popup has no clear
    /// room to show a smaller one in (see [`crate::popup::CLEAR_MARGINS`]):
    /// the panel fills the popup, so no pixel of it is left uncovered.
    fills: Option<(f32, f32)>,
    /// The row under the pointer or the keyboard, whose icon takes the
    /// accent, by its index in the level; `BACK_ROW` for the way back.
    hovered: Option<usize>,
    scroll: ScrollHandle,
    /// Letters typed towards a row, and when the last came.
    typed: String,
    typed_at: Option<Instant>,
}

type Answer<I> = Box<dyn FnOnce(Option<I>, &mut App)>;

impl<I: 'static> ControlHost for Menu<I> {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }
    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }
}

/// Closes the menu showing, if there is one. It answers `None`, as a
/// menu dismissed any other way does.
pub fn dismiss_open(cx: &mut App) {
    if let Some(open) = OPEN.with(|slot| slot.borrow_mut().take()) {
        (open.dismiss)(cx);
    }
}

/// Whether a floating menu is showing.
pub fn is_open() -> bool {
    OPEN.with(|slot| slot.borrow().is_some())
}

enum Trail {
    Nothing,
    Check,
    Chevron,
}

/// What a row shows, apart from where it is.
struct Look {
    label: SharedString,
    icon: Option<MenuIcon>,
    trail: Trail,
    shortcut: Option<SharedString>,
    enabled: bool,
    danger: bool,
}

impl Look {
    fn plain(label: SharedString, icon: Option<MenuIcon>, trail: Trail) -> Self {
        Look {
            label,
            icon,
            trail,
            shortcut: None,
            enabled: true,
            danger: false,
        }
    }
}

impl<I: Clone + 'static> Menu<I> {
    fn finish(&mut self, choice: Option<I>, window: &mut Window, cx: &mut App) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        self.watch = None;
        self._activation = None;
        let handle = window.window_handle();
        OPEN.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|open| open.window == handle) {
                *slot = None;
            }
        });
        platform::hide(window);
        window.remove_window();
        // After this update, not inside it: a menu dismissed because the
        // host opened another is closed while the host's window is busy,
        // and an answer given now could not reach it.
        if let Some(answer) = self.answer.take() {
            cx.defer(move |cx| answer(choice, cx));
        }
    }

    /// What the platform's watch, or a key in the window, has to say.
    pub(crate) fn signal(&mut self, signal: Signal, window: &mut Window, cx: &mut Context<Self>) {
        match signal {
            Signal::Close => self.finish(None, window, cx),
            Signal::Key(key) => self.key(key, window, cx),
            Signal::Text(text) => self.text(&text, cx),
        }
    }

    /// Shows the next level in the same popup, resizing it to fit.
    fn show_level(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(MenuItem::Submenu {
            label,
            items,
            enabled: true,
            ..
        }) = self.levels.last().and_then(|level| level.items.get(row))
        else {
            return;
        };
        self.levels.push(Level {
            items: items.clone(),
            title: Some(label.clone()),
        });
        self.change_level(Sweep::Forward, cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.change_level(Sweep::Back, cx);
        }
    }

    fn change_level(&mut self, sweep: Sweep, cx: &mut Context<Self>) {
        let level = self.levels.last().expect("menu has a level");
        (self.width, self.height) = self.fills.unwrap_or_else(|| {
            (
                width_of_level(level, |text, weight| measure(cx, text, weight)),
                height_of(level),
            )
        });
        self.reachable = reachable_rows(level);
        self.sweep = sweep;
        self.hovered = None;
        self.typed.clear();
        self.animation_epoch += 1;
        cx.notify();
    }

    /// Where the keyboard is, as a position among the reachable rows.
    fn at(&self) -> Option<usize> {
        let hovered = self.hovered?;
        self.reachable.iter().position(|&row| row == hovered)
    }

    /// Lights the reachable row at `at` and scrolls it into view.
    fn light(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(&row) = self.reachable.get(at) else {
            return;
        };
        self.hovered = Some(row);
        let back_rows = if self
            .levels
            .last()
            .is_some_and(|level| level.title.is_some())
        {
            2
        } else {
            0
        };
        self.scroll.scroll_to_item(row + back_rows);
        cx.notify();
    }

    /// Runs the lit row: an action answers with its id, a submenu opens.
    fn activate(&mut self, enter_only: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.at() else {
            return;
        };
        let row = self.reachable[at];
        match self.levels.last().and_then(|level| level.items.get(row)) {
            Some(MenuItem::Action(action)) if !enter_only => {
                let id = action.id.clone();
                self.finish(Some(id), window, cx)
            }
            Some(MenuItem::Submenu { .. }) => self.show_level(row, cx),
            _ => {}
        }
    }

    fn key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            Key::Dismiss => {
                if self.levels.len() > 1 {
                    self.back(cx);
                } else {
                    self.finish(None, window, cx);
                }
            }
            Key::Left => self.back(cx),
            Key::Activate => self.activate(false, window, cx),
            Key::Right => self.activate(true, window, cx),
            key => {
                if let Some(at) = step_row(key, self.at(), self.reachable.len()) {
                    self.light(at, cx);
                }
            }
        }
    }

    fn text(&mut self, text: &str, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self
            .typed_at
            .is_none_or(|last| now.duration_since(last) > TYPE_AHEAD_RESET)
        {
            self.typed.clear();
        }
        self.typed_at = Some(now);
        self.typed.push_str(text);
        let level = self.levels.last().expect("menu has a level");
        let labels: Vec<&str> = self
            .reachable
            .iter()
            .filter_map(|&row| level.items[row].label())
            .collect();
        if let Some(at) = type_ahead(&labels, &self.typed, self.at()) {
            self.light(at, cx);
        }
    }

    /// A row: `key` tells it apart for hovering, `icons` and `trailing` say
    /// whether the level keeps a column for an icon before the label and
    /// for a check or chevron after it.
    fn row(
        &self,
        key: usize,
        look: Look,
        (icons, trailing): (bool, bool),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette;
        let Look {
            label,
            icon,
            trail,
            shortcut,
            enabled,
            danger,
        } = look;
        let hovered = enabled && self.hovered == Some(key);
        let hover = if cx.reduce_motion() {
            self.controls
                .snap(("menu-row-hover", key), if hovered { 1.0 } else { 0.0 })
        } else {
            self.controls
                .blend(("menu-row-hover", key), hovered, slowed(HOVER))
        };
        let (rest, lit, wash) = if danger {
            (
                palette.danger_label,
                palette.danger_label,
                palette.danger_fill,
            )
        } else {
            (palette.text_secondary, palette.accent, palette.soft_fill)
        };
        let tint = color::lerp(rest, lit, hover);
        div()
            .id(("menu-row", key))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_INSET))
            .flex()
            .items_center()
            .gap(px(GAP))
            .rounded(px(ROW_RADIUS))
            .when(danger, |el| el.text_color(palette.danger_label))
            .when(hover > 0.0, |el| {
                el.bg(lighting::lit_at(
                    wash,
                    if danger { 0.08 } else { 0.035 },
                    hover,
                ))
            })
            .when(enabled, |el| el.cursor_pointer())
            .when(!enabled, |el| el.opacity(0.4))
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if *over {
                    this.hovered = Some(key);
                } else if this.hovered == Some(key) {
                    this.hovered = None;
                } else {
                    return;
                }
                cx.notify();
            }))
            .when(icons, |el| {
                el.child(
                    div()
                        .flex_none()
                        .w(px(ICON_SLOT))
                        .flex()
                        .justify_center()
                        .children(icon.map(|glyph| glyph.draw(ICON_SIZE, tint))),
                )
            })
            .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
            .children(shortcut.map(|shortcut| {
                div()
                    .flex_none()
                    .text_size(px(SHORTCUT_SIZE))
                    .text_color(palette.text_secondary)
                    .child(shortcut)
            }))
            .when(trailing, |el| {
                el.child(
                    div()
                        .flex_none()
                        .w(px(TRAILING_SLOT))
                        .flex()
                        .justify_center()
                        .map(|slot| match trail {
                            Trail::Nothing => slot,
                            Trail::Check => slot.child(tick(13.0, palette.accent)),
                            Trail::Chevron => slot.child(chevron(true, 11.0, tint)),
                        }),
                )
            })
    }
}

/// The lifted surface a floating panel sits on. The same recipe the
/// in-window menu uses for its panel, lightened a little further in the
/// dark scheme so it reads against a page.
fn raised(palette: Palette) -> Rgba {
    if palette.is_dark {
        color::lerp(palette.soft_fill, palette.soft_fill_hover, 0.6)
    } else {
        palette.field_surface
    }
}

impl<I: Clone + 'static> Render for Menu<I> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let palette = self.palette;
        let sweep = self.sweep;
        let level = self.levels.last().expect("menu has a level");
        let columns = columns(&level.items);
        let line = palette.area_border;
        let separator = move || {
            div()
                .flex_none()
                .h(px(1.0))
                .mx(px(ROW_INSET))
                .my(px((SEPARATOR_HEIGHT - 1.0) / 2.0))
                .bg(color::with_alpha(line, 0.8))
        };
        let mut rows = Vec::with_capacity(level.items.len() + 2);
        if let Some(title) = &level.title {
            // The way back, headed with the row that led here.
            let back = self
                .row(
                    BACK_ROW,
                    Look::plain(
                        title.clone(),
                        Some(MenuIcon::new(|size, color| chevron(false, size, color))),
                        Trail::Nothing,
                    ),
                    (true, columns.1),
                    cx,
                )
                .text_color(palette.text_secondary)
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(|this, _, _, cx| this.back(cx)));
            rows.push(back.into_any_element());
            rows.push(separator().into_any_element());
        }
        for (index, item) in level.items.iter().enumerate() {
            let row = match item {
                MenuItem::Separator => separator().into_any_element(),
                MenuItem::Header(label) => div()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .px(px(ROW_INSET))
                    .flex()
                    .items_center()
                    .text_size(px(SHORTCUT_SIZE))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.text_secondary)
                    .child(div().truncate().child(label.clone()))
                    .into_any_element(),
                MenuItem::Action(action) => {
                    let id = action.id.clone();
                    let look = Look {
                        label: action.label.clone(),
                        icon: action.icon.clone(),
                        trail: if action.checked {
                            Trail::Check
                        } else {
                            Trail::Nothing
                        },
                        shortcut: action.shortcut.clone(),
                        enabled: action.enabled,
                        danger: action.danger,
                    };
                    self.row(index, look, columns, cx)
                        .when(action.enabled, |el| {
                            el.on_click(cx.listener(move |this, _, window, cx| {
                                this.finish(Some(id.clone()), window, cx)
                            }))
                        })
                        .into_any_element()
                }
                MenuItem::Submenu {
                    label,
                    icon,
                    enabled,
                    ..
                } => {
                    let look = Look {
                        enabled: *enabled,
                        ..Look::plain(label.clone(), icon.clone(), Trail::Chevron)
                    };
                    self.row(index, look, columns, cx)
                        .when(*enabled, |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                this.show_level(index, cx);
                            }))
                        })
                        .into_any_element()
                }
            };
            rows.push(row);
        }
        if cx.reduce_motion() {
            self.controls.snap("menu-width", self.width);
            self.controls.snap("menu-height", self.height);
        }
        let width = self
            .controls
            .tween("menu-width", self.width, slowed(RESIZE));
        let height = self
            .controls
            .tween("menu-height", self.height, slowed(RESIZE));
        let (dx, dy) = sweep.offset();
        let rows = div()
            .id("menu-rows")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .p(px(PADDING))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);
        let rows = if self.animations_ready {
            rows.with_animation(
                ("menu-level-sweep", self.animation_epoch),
                Animation::new(slowed(ROW_SWEEP)).with_easing(crate::easing::ease_out_cubic),
                move |rows, t| {
                    rows.relative()
                        .left(px(dx * (1.0 - t)))
                        .top(px(dy * (1.0 - t)))
                        .opacity(t)
                },
            )
            .into_any_element()
        } else {
            rows.into_any_element()
        };
        let panel = div()
            .id("menu-panel")
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .flex_none()
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(RADIUS))
            .border_1()
            .border_color(color::with_alpha(palette.field_border_strong, 0.5))
            .bg(raised(palette))
            .shadow(lighting::panel(palette.is_dark))
            .child(rows);
        // Keep the native popup fixed; only its visible panel unrolls or
        // resizes. Animation IDs survive hover redraws and finish once.
        let panel = if self.animations_ready && sweep.unrolls() && self.fills.is_none() {
            panel
                .with_animation(
                    "menu-unroll",
                    Animation::new(slowed(SWEEP)).with_easing(crate::easing::ease_out_cubic),
                    move |panel, t| {
                        panel
                            .h(px(height * (0.35 + 0.65 * t)))
                            .opacity((t * 1.8).min(1.0))
                    },
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        };
        let root = keyboard::root(div().id("floating-menu"), self, cx)
            .size_full()
            .pt(px(TITLE_BAND))
            .pb(px(EDGE_GUTTER))
            .px(px(EDGE_GUTTER))
            .relative()
            .flex()
            .flex_col()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| this.finish(None, window, cx)),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, window, cx| this.finish(None, window, cx)),
            )
            // Where the window has the keyboard, keys arrive here; a chord
            // or a key the menu has no use for closes it, as a native menu
            // would. It goes no further: the host's window never had it.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let signal = match typed_event(event) {
                    Typed::Key(key) => Signal::Key(key),
                    Typed::Text(text) => Signal::Text(text),
                    Typed::Other => Signal::Close,
                };
                let taken = !matches!(signal, Signal::Close);
                this.signal(signal, window, cx);
                if taken {
                    cx.stop_propagation();
                }
            }))
            .when(self.anchored_above, |el| el.justify_end())
            .font_family(ui_font())
            .text_size(px(TEXT_SIZE))
            .text_color(palette.text_primary)
            .on_action(cx.listener(|this, _: &keyboard::Dismiss, window, cx| {
                this.key(Key::Dismiss, window, cx);
            }))
            .child(panel);
        if self.controls.animating() {
            window.request_animation_frame();
        }
        root
    }
}

/// Opens a menu of `items` with its top-left corner at `position` in
/// `window`, unrolling downward, and calls `on_choice` with the chosen
/// action's id — or `None` when the menu was dismissed — in `window`.
pub fn open_with<I: Clone + 'static>(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    items: Vec<MenuItem<I>>,
    palette: Palette,
    on_choice: impl FnOnce(Option<I>, &mut Window, &mut App) + 'static,
) {
    open_at(cx, window, position, items, palette, false, on_choice);
}

/// Opens a menu with its bottom edge at `position`, unrolling upward: for
/// a trigger on a shelf along the bottom of the window.
pub fn open_above_with<I: Clone + 'static>(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    items: Vec<MenuItem<I>>,
    palette: Palette,
    on_choice: impl FnOnce(Option<I>, &mut Window, &mut App) + 'static,
) {
    open_at(cx, window, position, items, palette, true, on_choice);
}

#[allow(clippy::too_many_arguments)]
fn open_at<I: Clone + 'static>(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    items: Vec<MenuItem<I>>,
    palette: Palette,
    anchored_above: bool,
    on_choice: impl FnOnce(Option<I>, &mut Window, &mut App) + 'static,
) {
    // One menu at a time.
    dismiss_open(cx);
    // The answer is given in the window the menu was opened for, if that
    // window is still open.
    let host = window.window_handle();
    let answer: Answer<I> = Box::new(move |choice, cx| {
        let _ = host.update(cx, move |_, window, cx| on_choice(choice, window, cx));
    });
    open_level(
        cx,
        crate::popup::content_origin(window) + position,
        window
            .display(cx)
            .map(|display| (display.id(), display.bounds())),
        vec![Level { items, title: None }],
        answer,
        palette,
        anchored_above,
    );
}

fn open_level<I: Clone + 'static>(
    cx: &mut App,
    pointer: Point<gpui::Pixels>,
    display: Option<(DisplayId, Bounds<gpui::Pixels>)>,
    levels: Vec<Level<I>>,
    answer: Answer<I>,
    palette: Palette,
    anchored_above: bool,
) {
    let Some(level) = levels.last() else {
        return;
    };
    let ((width, height), (popup_width, popup_height)) =
        level_sizes(level, |text, weight| measure(cx, text, weight));
    let fills = (!crate::popup::CLEAR_MARGINS).then_some((popup_width, popup_height));
    let (width, height) = fills.unwrap_or((width, height));
    let popup_height = popup_height + TITLE_BAND + EDGE_GUTTER;
    let popup_width = popup_width + 2.0 * EDGE_GUTTER;
    let initial_reachable = reachable_rows(level);
    let (origin, anchored_above) = place(
        point(f32::from(pointer.x), f32::from(pointer.y)),
        (popup_width, popup_height),
        display.map(|(_, screen)| {
            Bounds::new(
                point(f32::from(screen.origin.x), f32::from(screen.origin.y)),
                size(f32::from(screen.size.width), f32::from(screen.size.height)),
            )
        }),
        anchored_above,
    );
    let origin = point(px(origin.x), px(origin.y));
    let options = WindowOptions {
        display_id: display.map(|(id, _)| id),
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            origin,
            size(px(popup_width), px(popup_height)),
        ))),
        // Above everything. It takes the keyboard so the arrows reach it,
        // unless the platform's watch delivers them without.
        kind: WindowKind::PopUp,
        focus: !platform::KEYLESS,
        // Clear around the panel's rounded corners and while it unrolls.
        window_background: WindowBackgroundAppearance::Transparent,
        // Shown by `platform::shown` after a first hidden draw, which warms
        // the text and icons and settles the transparent gutter.
        show: false,
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    // A menu that cannot open answers as dismissed, so the caller is
    // never left waiting.
    let mut answer = Some(answer);
    let unanswered = &mut answer;
    let result = cx.open_window(options, move |window, cx| {
        let controls = ControlState::default();
        controls.snap("menu-width", width);
        controls.snap("menu-height", height);
        // Where the window has the keyboard, its root answers the keys.
        if !platform::KEYLESS {
            controls.focus_root(window, cx);
        }
        let menu = cx.new(|_| Menu {
            controls,
            palette,
            levels,
            reachable: initial_reachable,
            answer: unanswered.take(),
            watch: None,
            _activation: None,
            was_active: false,
            done: false,
            animations_ready: false,
            sweep: if anchored_above {
                Sweep::Up
            } else {
                Sweep::Down
            },
            anchored_above,
            animation_epoch: 0,
            width,
            height,
            fills,
            hovered: None,
            scroll: ScrollHandle::new(),
            typed: String::new(),
            typed_at: None,
        });
        // Going inactive after having been active is the reader clicking
        // elsewhere, or switching away: the menu closes, as a native one
        // would. A window with a platform watch leaves that to the watch,
        // which sees the click the window never gets.
        menu.update(cx, |menu, cx| {
            menu._activation = Some(cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                if this.watch.is_none() && this.was_active && !active {
                    this.finish(None, window, cx);
                }
                this.was_active |= active;
            }));
        });
        menu
    });
    let Ok(handle) = result else {
        if let Some(answer) = answer {
            cx.defer(move |cx| answer(None, cx));
        }
        return;
    };
    OPEN.with(|slot| {
        *slot.borrow_mut() = Some(Open {
            window: handle.into(),
            dismiss: Box::new(move |cx| {
                let _ = handle.update(cx, |menu, window, cx| menu.finish(None, window, cx));
            }),
        })
    });
    let watch = cx.update_window(handle.into(), |_, window, cx| {
        platform::watch(window, handle, cx)
    });
    let _ = handle.update(cx, |menu, _, cx| {
        menu.watch = watch.ok().flatten();
        menu.animations_ready = true;
        cx.notify();
    });
    // End the Menu borrow before showing the window, which draws it.
    let _ = cx.update_window(handle.into(), |_, window, cx| platform::shown(window, cx));
}

// ---- Glyphs -----------------------------------------------------------------

/// A stroked polyline on a 16-unit grid, `size` points square: the menu's
/// own chevrons and tick, drawn the way a host's icon set draws, so a row
/// with an icon and a row with a check sit at one weight.
fn polyline(points: &'static [(f32, f32)], size: f32, color: Rgba) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let scale = f32::from(bounds.size.width) / 16.0;
            let origin = bounds.origin;
            let mut builder = gpui::PathBuilder::stroke(px(1.5 * scale));
            for (n, &(x, y)) in points.iter().enumerate() {
                let at = point(origin.x + px(x * scale), origin.y + px(y * scale));
                if n == 0 {
                    builder.move_to(at);
                } else {
                    builder.line_to(at);
                }
            }
            if let Ok(path) = builder.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(size))
    .flex_none()
    .into_any_element()
}

fn chevron(right: bool, size: f32, color: Rgba) -> AnyElement {
    polyline(
        if right {
            &[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)]
        } else {
            &[(10.0, 3.5), (5.5, 8.0), (10.0, 12.5)]
        },
        size,
        color,
    )
}

fn tick(size: f32, color: Rgba) -> AnyElement {
    polyline(&[(3.25, 8.5), (6.5, 11.75), (12.75, 4.5)], size, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Item = MenuItem<&'static str>;

    fn level(items: Vec<Item>) -> Level<&'static str> {
        Level { items, title: None }
    }

    #[test]
    fn menus_are_as_tall_as_their_rows() {
        let mut level = level(vec![
            Item::action("a", "A"),
            Item::separator(),
            Item::header("Group"),
            Item::action("b", "B"),
        ]);
        let top = height_of(&level);
        assert_eq!(
            top,
            2.0 * ROW_HEIGHT + SEPARATOR_HEIGHT + HEADER_HEIGHT + 2.0 * (PADDING + BORDER)
        );
        level.title = Some("Back to".into());
        assert_eq!(height_of(&level), top + ROW_HEIGHT + SEPARATOR_HEIGHT);
    }

    #[test]
    fn menus_are_as_wide_as_their_longest_row() {
        let by_char = |text: &str, _: FontWeight| text.chars().count() as f32 * 7.0;
        let short = level(vec![Item::action("open", "Open")]);
        assert_eq!(width_of_level(&short, by_char), MIN_WIDTH);
        let label = "Clear local storage and databases for every site you have visited";
        let long = level(vec![Item::action("clear", label)]);
        assert_eq!(width_of_level(&long, by_char), MAX_WIDTH);
        // An icon column and a check column make room for themselves.
        let plain = level(vec![Item::action("close", "Close Tabs to the Right")]);
        let icon = MenuIcon::new(tick);
        let dressed = level(vec![
            Item::action("close", "Close Tabs to the Right")
                .checked(true)
                .icon(icon),
        ]);
        assert_eq!(
            width_of_level(&dressed, by_char) - width_of_level(&plain, by_char),
            ICON_SLOT + TRAILING_SLOT + 2.0 * GAP
        );
        // So does a shortcut after the label.
        let keyed = level(vec![
            Item::action("close", "Close Tabs to the Right").shortcut("⌘W"),
        ]);
        assert_eq!(
            width_of_level(&keyed, by_char) - width_of_level(&plain, by_char),
            by_char("⌘W", FontWeight::NORMAL) + GAP
        );
    }

    #[test]
    fn popup_fits_submenus_before_they_open() {
        let level = level(vec![Item::submenu(
            "Move to",
            vec![
                Item::action("a", "A longer destination than the parent row"),
                Item::action("b", "Another destination"),
            ],
        )]);
        let (_, (width, height)) = level_sizes(&level, |text, _| text.len() as f32 * 7.0);
        assert!(width > width_of_level(&level, |text, _| text.len() as f32 * 7.0));
        assert!(height > height_of(&level));
    }

    #[test]
    fn preparation_borrows_nested_labels_and_measures_each_level_once() {
        let icon = MenuIcon::new(tick);
        let level = Level {
            items: vec![
                Item::action("open", "Open"),
                Item::separator(),
                Item::submenu(
                    "Move to",
                    vec![
                        Item::action(
                            "far",
                            "A destination with a label wide enough to fill the panel",
                        )
                        .checked(true)
                        .icon(icon.clone()),
                        Item::submenu(
                            "Nested",
                            vec![Item::separator(), Item::action("here", "Here")],
                        ),
                    ],
                )
                .icon(icon),
            ],
            title: Some("Parent".into()),
        };
        let MenuItem::Action(open) = &level.items[0] else {
            unreachable!()
        };
        let MenuItem::Submenu {
            label: move_to,
            items,
            ..
        } = &level.items[2]
        else {
            unreachable!()
        };
        let MenuItem::Action(destination) = &items[0] else {
            unreachable!()
        };
        let MenuItem::Submenu {
            label: nested,
            items,
            ..
        } = &items[1]
        else {
            unreachable!()
        };
        let MenuItem::Action(here) = &items[1] else {
            unreachable!()
        };
        let parent = level.title.as_deref().unwrap();
        let calls = RefCell::new(Vec::new());
        let (root, maximum) = level_sizes(&level, |text, weight| {
            calls.borrow_mut().push((text.as_ptr(), weight));
            text.len() as f32 * 7.0
        });
        let expected_height = 3.0 * ROW_HEIGHT + 2.0 * SEPARATOR_HEIGHT + 2.0 * (PADDING + BORDER);
        assert_eq!(root, (MIN_WIDTH, expected_height));
        assert_eq!(maximum, (MAX_WIDTH, expected_height));
        assert_eq!(
            *calls.borrow(),
            vec![
                (open.label.as_ptr(), FontWeight::NORMAL),
                (move_to.as_ptr(), FontWeight::NORMAL),
                (parent.as_ptr(), FontWeight::MEDIUM),
                (destination.label.as_ptr(), FontWeight::NORMAL),
                (nested.as_ptr(), FontWeight::NORMAL),
                (move_to.as_ptr(), FontWeight::MEDIUM),
                (here.label.as_ptr(), FontWeight::NORMAL),
                (nested.as_ptr(), FontWeight::MEDIUM),
            ]
        );
    }

    #[test]
    fn the_keyboard_skips_separators_headers_and_disabled_rows() {
        let level = level(vec![
            Item::action("open", "Open"),
            Item::separator(),
            Item::header("More"),
            Item::action("closed", "Closed").disabled(),
            Item::submenu("Move to", vec![]),
            Item::submenu("Nowhere", vec![]).disabled(),
        ]);
        assert_eq!(reachable_rows(&level), vec![0, 4]);
        assert_eq!(step_row(Key::Down, None, 2), Some(0));
        assert_eq!(step_row(Key::Up, None, 2), Some(1));
        assert_eq!(step_row(Key::Down, Some(1), 2), Some(0));
        assert_eq!(step_row(Key::Down, None, 0), None);
    }

    #[test]
    fn keys_are_sorted_and_shortcuts_are_let_through() {
        assert_eq!(typed(Some(Key::Down), None, false), Typed::Key(Key::Down));
        assert_eq!(
            typed(Some(Key::Activate), Some("\r"), false),
            Typed::Key(Key::Activate)
        );
        assert_eq!(typed(None, Some("a"), false), Typed::Text("a".into()));
        assert_eq!(typed(None, Some("a"), true), Typed::Other);
        assert_eq!(typed(None, Some("\t"), false), Typed::Other);
        assert_eq!(typed(Some(Key::Down), None, true), Typed::Other);
        assert_eq!(typed(None, None, false), Typed::Other);
    }

    #[test]
    fn typing_finds_a_row_and_a_repeated_letter_cycles() {
        let labels = ["New Tab", "New Window", "Close", "Copy Link", "Cut"];
        assert_eq!(type_ahead(&labels, "n", None), Some(0));
        assert_eq!(type_ahead(&labels, "ne", None), Some(0));
        assert_eq!(type_ahead(&labels, "new w", None), Some(1));
        assert_eq!(type_ahead(&labels, "c", Some(0)), Some(2));
        assert_eq!(type_ahead(&labels, "cc", Some(2)), Some(3));
        assert_eq!(type_ahead(&labels, "ccc", Some(4)), Some(2));
        assert_eq!(type_ahead(&labels, "x", None), None);
        assert_eq!(type_ahead(&[], "x", None), None);
    }

    #[test]
    fn a_menu_near_the_bottom_opens_upward_or_is_slid_back_in() {
        let screen = Some(Bounds::new(point(0.0, 0.0), size(1440.0, 900.0)));
        let popup = (200.0, 300.0);
        // Room below: the panel's top edge sits at the pointer.
        let (origin, above) = place(point(100.0, 100.0), popup, screen, false);
        assert_eq!(
            (origin, above),
            (point(100.0 - EDGE_GUTTER, 100.0 - TITLE_BAND), false)
        );
        // No room below but room above: it flips, bottom edge at the pointer.
        let (origin, above) = place(point(100.0, 850.0), popup, screen, false);
        assert!(above);
        assert_eq!(origin.y, 850.0 - (300.0 - EDGE_GUTTER));
        // Room neither way: slid back to the top edge rather than flipped.
        let short = Some(Bounds::new(point(0.0, 0.0), size(1440.0, 200.0)));
        let (origin, above) = place(point(100.0, 190.0), popup, short, false);
        assert!(!above);
        assert_eq!(origin.y, 0.0);
        // Asked to open upward, it does, and stays on the screen.
        let (origin, above) = place(point(100.0, 100.0), popup, screen, true);
        assert!(above);
        assert_eq!(origin.y, 0.0);
        // The right edge is kept too.
        let (origin, _) = place(point(1400.0, 100.0), popup, screen, false);
        assert_eq!(origin.x, 1240.0);
    }
}
