//! The mutable bits the controls need between frames.
//!
//! Most of a control is a pure function of the data it is handed. What is
//! left over is small and always the same shape: which pop-up is open and
//! how far into its fade it is, when each animated control last changed, and
//! whatever is being dragged. A host keeps one [`ControlState`], implements
//! [`ControlHost`] for its view, and every control is generic over that
//! host, so nothing here knows the host's type.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use std::cell::{Cell, RefCell};

use gpui::{
    App, Bounds, Context, ElementId, FocusHandle, MouseMoveEvent, Pixels, Point, ScrollHandle,
    SharedString, Window,
};

pub use crate::scroll::{ScrollAxis, ScrollDrag, apply_scroll_drag};

/// A pop-up list fades and slides into place over this long, and back out
/// over the same.
pub const COMBO_REVEAL: Duration = Duration::from_millis(140);
/// A switch's knob and track take this long to cross over. Disclosure
/// chevrons and other small state changes use it too, so everything in a
/// view turns at one speed.
pub const SWITCH_SLIDE: Duration = Duration::from_millis(140);
/// Something moving from one place to another — a selection pill sliding
/// along its segments, a row taking its new place after a sort — takes
/// this long. A little slower than a state flip, because the eye has to
/// follow it there.
pub const MOVE: Duration = Duration::from_millis(180);
/// A whole scheme crossing over — light to dark, one hue to another. The
/// slowest thing here: everything on screen is changing at once, and a
/// fast cut of that reads as a flash.
pub const SCHEME_FADE: Duration = Duration::from_millis(240);

/// A value on its way from where it was to where it has been asked to be.
#[derive(Clone, Copy, Debug)]
struct Tween {
    from: f32,
    to: f32,
    since: Instant,
    duration: Duration,
    /// The frame it was last asked about; see [`ControlState::animating`].
    touched: u64,
}

impl Tween {
    fn value(&self) -> f32 {
        if !self.running() {
            return self.to;
        }
        let t = crate::easing::ease_out_cubic(crate::easing::progress(self.since, self.duration));
        self.from + (self.to - self.from) * t
    }

    fn running(&self) -> bool {
        self.since.elapsed() < self.duration
    }
}

/// Identifies one pop-up, menu, track or tab bar. An element id doubles as
/// its identity, so a host needs no enum of its own. Two of the same kind of
/// widget in one view must not share one.
pub type ComboId = &'static str;

/// Which way a drag reads its position out of a track.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackAxis {
    Horizontal,
    Vertical,
    /// A two-dimensional pad, such as a colour field.
    Both,
}

/// A drag along some widget's track: a slider handle, a split divider, a
/// colour pad. All of them are the same gesture over different geometry, so
/// they share one mechanism.
#[derive(Clone, Debug)]
pub struct TrackDrag {
    pub id: ComboId,
    pub axis: TrackAxis,
    /// Evenly spaced stops to snap to, if the track has any.
    pub stops: Option<u32>,
}

/// A tab being dragged along its bar.
#[derive(Clone, Debug)]
pub struct TabDrag {
    pub bar: ComboId,
    /// Where the tab started.
    pub from: usize,
    /// Where it would land if released now.
    pub to: usize,
    /// Whether the pointer has actually moved. A press that never moves is a
    /// click on the tab, not a reorder.
    pub moved: bool,
    /// How far into the tab the pointer took hold, in window x, so the tab
    /// rides under the hand where it was grabbed rather than by its edge.
    pub grab: f32,
    /// Where the pointer is now, in window x.
    pub pointer: f32,
}

/// A context menu that is open: which menu, where it was summoned, and what
/// it was summoned on.
#[derive(Clone, Debug)]
pub struct OpenMenu {
    pub id: ComboId,
    /// Where the pointer was, in window coordinates. The menu opens here.
    pub anchor: Point<Pixels>,
    /// Optional element the menu hangs beneath instead, for a menu button.
    pub under: Option<Bounds<Pixels>>,
    /// What was clicked, in whatever form the host wants it back: a row id,
    /// a file path, a row id. The menu never looks inside it.
    pub target: SharedString,
    pub opened_at: Instant,
    /// Which row the keyboard is on, counting only the rows that can be
    /// activated — headers and separators are passed over rather than landed
    /// on, because arrowing onto something inert reads as a stuck key.
    pub highlight: Option<usize>,
}

/// Per-view control state. `Default` is the empty state, which is also the
/// right starting point: nothing open, nothing animating, nothing dragged.
#[derive(Default)]
pub struct ControlState {
    /// The pop-up whose list is open, if any.
    pub open_combo: Option<ComboId>,
    /// When that list started revealing.
    pub combo_opened_at: Option<Instant>,
    /// A list fading back out, and when that began. It keeps rendering,
    /// without taking clicks, until the fade finishes.
    pub combo_closing: Option<(ComboId, Instant)>,
    /// One focus handle per composite control, keyed by its id.
    ///
    /// A group — radio buttons, a segmented control, a tab bar, a tree — is
    /// one stop in the tab order, and the ring belongs on whichever option
    /// inside it is current rather than around the whole group. That means
    /// the handle has to move from option to option as the selection moves,
    /// which it can only do if it outlives all of them: a handle owned by an
    /// option dies the moment that option stops being the current one, and
    /// takes the keyboard with it after a single arrow press.
    ///
    /// A `RefCell` because this is a cache, not state. A control asks for
    /// its handle while it renders, holding only `&ControlState`, and the
    /// answer is the same handle every frame.
    focus_handles: RefCell<HashMap<ElementId, FocusHandle>>,
    /// The handle the open context menu holds focus with, and where focus
    /// was before it opened.
    ///
    /// A menu has to hold focus to hear the arrows, and the handle has to be
    /// the same one every frame — a fresh handle per render would lose focus
    /// the instant it was given. Created once, on the first menu opened, and
    /// reused after that.
    pub menu_focus: Option<FocusHandle>,
    /// Where the keyboard was before the menu took it, so closing puts it
    /// back rather than dropping the reader at the top of the window.
    pub menu_return_focus: Option<FocusHandle>,
    /// The same, for a modal dialog.
    pub dialog_return_focus: Option<FocusHandle>,
    /// The focus handles of the open dialog's buttons, refreshed every frame
    /// it renders and cleared when it does not. What Tab stays inside of
    /// while a dialog is up; see [`crate::keyboard::move_focus`].
    dialog_trap: RefCell<Option<Vec<FocusHandle>>>,
    /// Which option the keyboard is on while a pop-up list is open.
    ///
    /// Not the selection: a list you are arrowing through has not chosen
    /// anything yet, and closing it with Escape has to leave the old value
    /// alone. It commits on Enter and is dropped when the list closes.
    pub combo_highlight: Option<usize>,
    /// A pop-up just closed by a press outside it. The same press's click
    /// must not reopen it when it landed on that pop-up's own button.
    pub combo_dismissed: Option<ComboId>,
    /// When `combo_dismissed` was set. The release that should clear it can
    /// land on an occluding surface and never reach the host, so the marker
    /// expires on its own too.
    pub combo_dismissed_at: Option<Instant>,

    /// When each two-state element last changed, and how long its change
    /// takes. Absent means "never changed", which is what keeps a first
    /// paint or a remount from animating. Behind a `RefCell` because the
    /// change is noticed during render, from `&ControlState`: see
    /// [`ControlState::transition`].
    pub anim: RefCell<HashMap<ElementId, (Instant, Duration)>>,
    /// The value each two-state element showed last frame, so a change can
    /// be noticed whoever made it, and the frame it was last rendered in.
    seen: RefCell<HashMap<ElementId, (u64, u64)>>,
    /// Continuous values in flight: a pill's x, a row's y, a scheme's
    /// darkness. See [`ControlState::tween`].
    tweens: RefCell<HashMap<ElementId, Tween>>,
    /// Which frame each group of rows first appeared in, for the lists that
    /// fade new rows in: see [`ControlState::present`].
    streaks: RefCell<HashMap<ElementId, (u64, u64)>>,
    /// Frames counted by [`ControlState::animating`], which is called once
    /// per render. Everything above that stops being rendered is retired a
    /// frame or two later, so a row that leaves and comes back is new again.
    frame: Cell<u64>,
    /// Every duration is multiplied by this; see
    /// [`ControlState::set_time_scale`]. `None` is one.
    time_scale: Option<f32>,
    /// One scroll handle per pop-up list, so a list can read its own offset
    /// and fade its edges. Same reasoning as `focus_handles`.
    scroll_handles: RefCell<HashMap<ElementId, ScrollHandle>>,

    /// Scrollbar thumb drag in flight.
    pub scroll_drag: Option<ScrollDrag>,

    /// Slider, split or colour-pad drag in flight.
    pub track_drag: Option<TrackDrag>,
    /// Each track's bounds, recorded as it paints. A drag needs them long
    /// after the pointer has left the track.
    pub track_bounds: HashMap<ComboId, Bounds<Pixels>>,

    /// Tab reorder in flight.
    pub tab_drag: Option<TabDrag>,
    /// Each tab bar's per-tab bounds, in bar order, recorded as they paint.
    pub tab_bounds: HashMap<ComboId, Vec<Bounds<Pixels>>>,
    /// Each tab's width by the tab's own id rather than by slot or index, so
    /// a bar can work out where every tab will land in a new order before it
    /// has been laid out there, and slide it over — and so a reorder, which
    /// renumbers every index, changes nobody's record.
    pub tab_widths: HashMap<ComboId, HashMap<SharedString, f32>>,
    /// Bounds of each option of a segmented control, in option order, and
    /// of the group around them, recorded as they paint. The selection pill
    /// slides between these.
    pub slot_bounds: HashMap<ComboId, Vec<Bounds<Pixels>>>,
    pub group_bounds: HashMap<ComboId, Bounds<Pixels>>,
    /// Bounds of anything that asked to be measured as it painted, by key:
    /// a disclosure's body, so it can be shown growing to the height it will
    /// have rather than appearing at it.
    pub measured: HashMap<ElementId, Bounds<Pixels>>,

    /// The open context menu, if any.
    pub menu: Option<OpenMenu>,

    /// The shortcut recorder waiting for a key chord, if any.
    pub recording: Option<ComboId>,
}

impl ControlState {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- Pop-ups ----

    pub fn is_combo_open(&self, combo: ComboId) -> bool {
        self.open_combo == Some(combo)
    }

    /// Closes the open pop-up, letting its list fade back out the way it
    /// faded in.
    pub fn close_combo(&mut self) {
        self.combo_highlight = None;
        if let Some(combo) = self.open_combo.take() {
            self.combo_closing = Some((combo, Instant::now()));
        }
    }

    /// Opens one pop-up, closing whichever was open.
    pub fn open_combo(&mut self, combo: ComboId) {
        self.close_combo();
        self.open_combo = Some(combo);
        self.combo_opened_at = Some(Instant::now());
        self.combo_closing = None;
    }

    /// Puts the keyboard on one option of the open list. Opening a pop-up
    /// starts it on whatever is already chosen, so the first arrow press
    /// moves from there rather than from the top.
    pub fn highlight_combo(&mut self, index: usize) {
        self.combo_highlight = Some(index);
    }

    /// Which option the keyboard is on, falling back to `selected` for a
    /// list opened with the mouse and never arrowed through.
    pub fn combo_highlight_or(&self, selected: usize) -> usize {
        self.combo_highlight.unwrap_or(selected)
    }

    // ---- Animation ----

    /// Slows every animation down by `scale` — eight makes a 140ms slide
    /// take over a second. For looking at motion: a screenshot taken during
    /// a slide is the only way to see what the slide actually does, and at
    /// full speed the shutter is slower than the slide. The gallery reads
    /// `VAMPIR_SLOW_MOTION` into this.
    pub fn set_time_scale(&mut self, scale: f32) {
        self.time_scale = (scale > 0.0 && scale != 1.0).then_some(scale);
    }

    /// A duration at the current time scale.
    pub fn scaled(&self, duration: Duration) -> Duration {
        match self.time_scale {
            Some(scale) => duration.mul_f32(scale),
            None => duration,
        }
    }

    /// Marks an element as having just changed state, starting its
    /// animation. For a control that renders a value, prefer
    /// [`ControlState::transition`], which notices the change itself.
    pub fn mark_changed(&self, id: impl Into<ElementId>) {
        self.anim
            .borrow_mut()
            .insert(id.into(), (Instant::now(), self.scaled(SWITCH_SLIDE)));
    }

    /// How far through its animation an element is, 0..=1. An element that
    /// never changed reads as finished, so it paints its end state.
    pub fn anim_progress(&self, id: &ElementId, duration: Duration) -> f32 {
        self.anim
            .borrow()
            .get(id)
            .map(|(since, _)| crate::easing::progress(*since, self.scaled(duration)))
            .unwrap_or(1.0)
    }

    /// Progress of `id`'s transition to `value`, 0..=1, starting one when
    /// the value is not what it showed last frame.
    ///
    /// Called from render rather than from the click handler, so a switch
    /// flipped by a menu item, a shortcut, a command palette or the host's
    /// own code slides exactly as one flipped by the pointer does. The
    /// animation is about the state changing, not about who changed it. The
    /// first time an element is seen it paints its end state: nothing should
    /// animate in from nowhere on the first frame.
    ///
    /// Because the change is noticed *during* render, a host has to ask
    /// [`ControlState::animating`] after its controls have been built, not
    /// before — otherwise the frame that starts a slide never asks for the
    /// frame that would continue it, and the control sits at the start of
    /// its slide until something else repaints. GPUI ignores a `notify` from
    /// inside render, so the control cannot ask on the host's behalf.
    pub fn transition(&self, id: &ElementId, value: u64, duration: Duration) -> f32 {
        let frame = self.frame.get();
        let previous = self.seen.borrow_mut().insert(id.clone(), (value, frame));
        if previous.is_some_and(|(previous, _)| previous != value) {
            self.anim
                .borrow_mut()
                .insert(id.clone(), (Instant::now(), self.scaled(duration)));
        }
        self.anim_progress(id, duration)
    }

    /// Where a two-state element is between its off (0) and on (1) looks,
    /// eased, given whether it is on now. The companion to
    /// [`ControlState::transition`] for the common case: a control mixes its
    /// two looks by this number and gets its slide in either direction, from
    /// wherever it was when the state flipped back.
    pub fn blend(&self, id: &ElementId, on: bool, duration: Duration) -> f32 {
        let t = crate::easing::ease_out_cubic(self.transition(id, u64::from(on), duration));
        if on { t } else { 1.0 - t }
    }

    /// A continuous value on its way to `target`: the pill under a segmented
    /// control's selection, a row's place in a re-sorted table, the darkness
    /// of a scheme. Returns what to draw this frame.
    ///
    /// Asked for a new target it sets off from wherever it is now, so a
    /// change of mind mid-slide bends the motion rather than restarting it.
    /// The first time an id is seen it is already at its target: nothing
    /// slides in from nowhere on the first frame. Like `transition`, this is
    /// noticed during render, so the host asks [`ControlState::animating`]
    /// after its controls have been built.
    pub fn tween(&self, id: impl Into<ElementId>, target: f32, duration: Duration) -> f32 {
        self.tween_inner(id.into(), None, target, duration)
    }

    /// [`ControlState::tween`], but an id seen for the first time starts at
    /// `initial` and slides to `target` — for a row fading in when it joins
    /// a list. Pair it with [`ControlState::present`] so the first frame of
    /// the whole list is not a hundred rows fading in at once.
    pub fn tween_from(
        &self,
        id: impl Into<ElementId>,
        initial: f32,
        target: f32,
        duration: Duration,
    ) -> f32 {
        self.tween_inner(id.into(), Some(initial), target, duration)
    }

    /// A tween for an angle in degrees, which goes the short way round: from
    /// 350 to 10 is twenty degrees, not three hundred and forty.
    pub fn tween_angle(&self, id: impl Into<ElementId>, degrees: f32, duration: Duration) -> f32 {
        let id = id.into();
        let target = match self.tweens.borrow().get(&id) {
            Some(tween) => {
                let here = tween.to;
                let delta = (degrees - here).rem_euclid(360.0);
                here + if delta > 180.0 { delta - 360.0 } else { delta }
            }
            None => degrees,
        };
        self.tween_inner(id, None, target, duration)
    }

    /// Puts a tween at `value` at once, with no slide. For a value the hand
    /// is dragging: the pointer is the animation, and a control that trails
    /// it reads as lag.
    pub fn snap(&self, id: impl Into<ElementId>, value: f32) -> f32 {
        let id = id.into();
        self.tweens.borrow_mut().insert(
            id,
            Tween {
                from: value,
                to: value,
                since: Instant::now(),
                duration: Duration::ZERO,
                touched: self.frame.get(),
            },
        );
        value
    }

    fn tween_inner(
        &self,
        id: ElementId,
        initial: Option<f32>,
        target: f32,
        duration: Duration,
    ) -> f32 {
        let duration = self.scaled(duration);
        let frame = self.frame.get();
        let mut tweens = self.tweens.borrow_mut();
        let tween = tweens.entry(id).or_insert_with(|| match initial {
            Some(from) => Tween {
                from,
                to: target,
                since: Instant::now(),
                duration,
                touched: frame,
            },
            None => Tween {
                from: target,
                to: target,
                since: Instant::now(),
                duration: Duration::ZERO,
                touched: frame,
            },
        });
        tween.touched = frame;
        if tween.to != target {
            tween.from = tween.value();
            tween.to = target;
            tween.since = Instant::now();
            tween.duration = duration;
        }
        tween.value()
    }

    /// Whether a group of rows — a list, a tree, a menu — was already on
    /// screen last frame. Call it once per row with the *group's* id.
    ///
    /// A row joining a list that is already up should fade in; a list that
    /// has just appeared as a whole should not fade in row by row, it is
    /// arriving with whatever brought it. This tells the two apart, and it
    /// answers the same for every row in a frame however many ask.
    pub fn present(&self, group: impl Into<ElementId>) -> bool {
        let frame = self.frame.get();
        let mut streaks = self.streaks.borrow_mut();
        let entry = streaks.entry(group.into()).or_insert((frame, frame));
        // (started, touched): a gap of more than a frame ends the streak.
        if entry.1 + 1 < frame {
            entry.0 = frame;
        }
        entry.1 = frame;
        entry.0 < frame
    }

    /// The scroll handle for a pop-up list, made the first time it is asked
    /// for and the same one thereafter, so the list can read its own offset
    /// and fade the edge that has something past it.
    pub fn scroll(&self, id: impl Into<ElementId>) -> ScrollHandle {
        self.scroll_handles
            .borrow_mut()
            .entry(id.into())
            .or_default()
            .clone()
    }

    /// True while a fade or slide still needs frames. A host folds this into
    /// whatever decides to request the next frame.
    ///
    /// Also the end of a frame's bookkeeping: it is called once per render,
    /// after everything has been built, so it counts frames, and retires the
    /// records of anything that has not been rendered for two of them. That
    /// is what lets a row that leaves a list and comes back be new again, and
    /// it is why a host that skips calling this leaks nothing worse than a
    /// few stale entries.
    pub fn animating(&self) -> bool {
        let frame = self.frame.get();
        self.frame.set(frame + 1);
        let stale = |touched: u64| touched + 1 < frame;
        self.tweens
            .borrow_mut()
            .retain(|_, tween| !stale(tween.touched));
        self.seen
            .borrow_mut()
            .retain(|_, (_, touched)| !stale(*touched));
        self.streaks
            .borrow_mut()
            .retain(|_, (_, touched)| !stale(*touched));

        if self
            .anim
            .borrow()
            .values()
            .any(|(since, duration)| since.elapsed() < *duration)
        {
            return true;
        }
        if self.tweens.borrow().values().any(Tween::running) {
            return true;
        }
        let reveal = self.scaled(COMBO_REVEAL);
        if self.open_combo.is_some()
            && self
                .combo_opened_at
                .is_some_and(|since| since.elapsed() < reveal)
        {
            return true;
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.opened_at.elapsed() < reveal)
        {
            return true;
        }
        self.combo_closing
            .is_some_and(|(_, since)| since.elapsed() < reveal)
    }

    /// Drops animations that have finished. Optional housekeeping: the map
    /// is keyed by element id and so is bounded by the number of animated
    /// controls, but a host that builds ids from data can call this to keep
    /// it from growing.
    pub fn prune_anims(&mut self) {
        self.anim
            .borrow_mut()
            .retain(|_, (since, duration)| since.elapsed() < *duration);
    }

    // ---- Track drags ----

    pub fn scroll_dragging(&self) -> bool {
        self.scroll_drag.is_some()
    }

    pub fn track_dragging(&self) -> bool {
        self.track_drag.is_some()
    }

    /// Whether a particular track is the one being dragged.
    pub fn is_dragging(&self, id: ComboId) -> bool {
        self.track_drag.as_ref().is_some_and(|drag| drag.id == id)
    }

    /// Starts a drag on a track, recording how it should be read.
    pub fn begin_track_drag(&mut self, id: ComboId, axis: TrackAxis, stops: Option<u32>) {
        self.track_drag = Some(TrackDrag { id, axis, stops });
    }

    /// Where along the dragged track a pointer position falls, each
    /// component 0..=1 and already snapped to the track's stops. `None`
    /// when nothing is being dragged, or when the track has not painted yet
    /// and so has no bounds.
    ///
    /// Both components are always filled in; a horizontal track's `y` is
    /// simply not interesting.
    pub fn track_ratio_at(&self, position: Point<Pixels>) -> Option<(ComboId, Point<f32>)> {
        let drag = self.track_drag.as_ref()?;
        let bounds = self.track_bounds.get(drag.id)?;
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let snap = |ratio: f32| match drag.stops {
            Some(stops) if stops >= 2 => {
                let last = (stops - 1) as f32;
                (ratio * last).round() / last
            }
            _ => ratio,
        };
        let x = ((f32::from(position.x) - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0);
        let y = ((f32::from(position.y) - f32::from(bounds.origin.y)) / height).clamp(0.0, 1.0);
        // A pad snaps on both axes; a one-dimensional track only on its own,
        // so the unused component stays exact for a host that reads it.
        let point = match drag.axis {
            TrackAxis::Horizontal => Point::new(snap(x), y),
            TrackAxis::Vertical => Point::new(x, snap(y)),
            TrackAxis::Both => Point::new(snap(x), snap(y)),
        };
        Some((drag.id, point))
    }

    // ---- Tab drags ----

    /// Which tab in a bar sits under a pointer position, if any.
    pub fn tab_at(&self, bar: ComboId, position: Point<Pixels>) -> Option<usize> {
        self.tab_bounds
            .get(bar)?
            .iter()
            .position(|bounds| bounds.contains(&position))
    }

    /// Updates a tab drag for a new pointer position, returning whether
    /// anything on screen should change: the held tab rides with the pointer,
    /// so every move during a drag is a redraw, and some moves also change
    /// the landing slot.
    ///
    /// A tab moves only once the pointer is past the midpoint of its
    /// neighbour, so a tab never swaps back and forth under a still hand.
    pub fn drag_tab_to(&mut self, position: Point<Pixels>) -> bool {
        let pointer = f32::from(position.x);
        let Some(drag) = self.tab_drag.as_mut() else {
            return false;
        };
        drag.pointer = pointer;
        drag.moved = true;
        let (bar, to) = (drag.bar, drag.to);
        let Some(bounds) = self.tab_bounds.get(bar) else {
            return true;
        };
        let Some(current) = bounds.get(to) else {
            return true;
        };
        let mut target = None;
        if pointer < f32::from(current.left())
            && to > 0
            && let Some(previous) = bounds.get(to - 1)
            && pointer < f32::from(previous.center().x)
        {
            target = Some(to - 1);
        }
        if pointer > f32::from(current.right())
            && let Some(next) = bounds.get(to + 1)
            && pointer > f32::from(next.center().x)
        {
            target = Some(to + 1);
        }
        if let Some(to) = target {
            self.tab_drag.as_mut().expect("checked above").to = to;
        }
        true
    }

    // ---- Context menus ----

    /// Opens a context menu at a pointer position. `target` is the host's
    /// own note of what was clicked; it comes back untouched.
    pub fn open_menu(
        &mut self,
        id: ComboId,
        anchor: Point<Pixels>,
        target: impl Into<SharedString>,
    ) {
        self.close_combo();
        self.menu = Some(OpenMenu {
            id,
            anchor,
            under: None,
            target: target.into(),
            opened_at: Instant::now(),
            highlight: None,
        });
    }

    /// Opens a menu hanging beneath an element rather than at the pointer,
    /// for a button that drops a menu.
    pub fn open_menu_under(
        &mut self,
        id: ComboId,
        under: Bounds<Pixels>,
        target: impl Into<SharedString>,
    ) {
        self.close_combo();
        self.menu = Some(OpenMenu {
            id,
            anchor: under.origin,
            under: Some(under),
            target: target.into(),
            opened_at: Instant::now(),
            highlight: None,
        });
    }

    /// The focus handle for a composite control, made the first time it is
    /// asked for and the same one thereafter.
    ///
    /// Always a tab stop: a group is exactly one stop however many options
    /// it holds, which is why Tab passes a twenty-tab bar in one press.
    pub fn focus(&self, id: impl Into<ElementId>, cx: &App) -> FocusHandle {
        self.focus_handles
            .borrow_mut()
            .entry(id.into())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// Records, or clears, the buttons a modal dialog is keeping the keyboard
    /// among. Called by [`crate::containers::dialog`] each frame.
    pub fn set_dialog_trap(&self, buttons: Option<Vec<FocusHandle>>) {
        *self.dialog_trap.borrow_mut() = buttons;
    }

    /// Where Tab goes next while a modal dialog holds the keyboard, or `None`
    /// when none does.
    ///
    /// Only answers while the keyboard is actually on one of the dialog's
    /// buttons. If it is anywhere else the dialog has either not taken it
    /// yet or has already gone, and in neither case is the press ours.
    pub fn trap_next(&self, window: &Window, backward: bool) -> Option<FocusHandle> {
        let trap = self.dialog_trap.borrow();
        let buttons = trap.as_ref()?;
        let here = buttons
            .iter()
            .position(|handle| handle.is_focused(window))?;
        let count = buttons.len();
        let next = if backward {
            (here + count - 1) % count
        } else {
            (here + 1) % count
        };
        Some(buttons[next].clone())
    }

    /// Hands the keyboard back to whatever had it before a dialog opened.
    ///
    /// The dialog's own buttons and scrim do this themselves. A host that
    /// closes the dialog some other way — Escape through its own action,
    /// say — calls it, or focus is left on a button that no longer exists
    /// and the next shortcut has nowhere to arrive.
    pub fn restore_dialog_focus(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.dialog_return_focus.take() {
            window.focus(&handle, cx);
        }
    }

    /// The focus handle the open menu uses, creating it the first time a
    /// menu is opened in this view.
    pub fn menu_focus(&mut self, cx: &App) -> FocusHandle {
        self.menu_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Puts the keyboard on one row of the open menu.
    pub fn highlight_menu(&mut self, index: Option<usize>) {
        if let Some(menu) = self.menu.as_mut() {
            menu.highlight = index;
        }
    }

    /// Which row of the open menu the keyboard is on.
    pub fn menu_highlight(&self) -> Option<usize> {
        self.menu.as_ref().and_then(|menu| menu.highlight)
    }

    /// Closes whatever is showing over the view: a pop-up list, a context
    /// menu.
    ///
    /// Call it when the keyboard commits to something else. A dropdown is a
    /// question the view is asking, and moving to another control answers it
    /// by walking away — leaving the list on screen makes it look as though
    /// the next key pressed will land in it.
    pub fn dismiss_popups(&mut self) {
        self.close_combo();
        self.close_menu();
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    pub fn is_menu_open(&self, id: ComboId) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.id == id)
    }

    /// What the open menu was summoned on, if any.
    pub fn menu_target(&self) -> Option<&str> {
        self.menu.as_ref().map(|menu| menu.target.as_ref())
    }

    /// When the open menu was summoned, if any.
    ///
    /// This identifies one *opening* rather than one menu, which is what
    /// lets a caller tell "still the menu I was in" apart from "the callback
    /// I just ran opened another one".
    pub fn menu_opened_at(&self) -> Option<Instant> {
        self.menu.as_ref().map(|menu| menu.opened_at)
    }

    // ---- Drag lifecycle ----

    /// Whether any gesture owned by the controls is in flight. A host with
    /// drags of its own ORs this with them.
    pub fn dragging_anything(&self) -> bool {
        self.scroll_drag.is_some() || self.track_drag.is_some() || self.tab_drag.is_some()
    }

    /// Ends every drag and clears the dismissal marker. Call it from the
    /// host's mouse-up, and from a mouse-move with no button held: a release
    /// outside the window never arrives, so without that a drag would still
    /// be running when the pointer comes back.
    ///
    /// Returns the finished tab reorder, if the gesture was one and it
    /// actually moved, so the host can apply it.
    pub fn end_drag(&mut self) -> Option<(ComboId, usize, usize)> {
        self.scroll_drag = None;
        self.track_drag = None;
        // A marker no toggle consumed (the click landed elsewhere) must not
        // eat some later toggle click.
        self.combo_dismissed = None;
        self.combo_dismissed_at = None;
        self.tab_drag
            .take()
            .filter(|drag| drag.moved && drag.from != drag.to)
            .map(|drag| (drag.bar, drag.from, drag.to))
    }
}

/// A view that hosts vampir controls.
///
/// The two required methods hand out the view's [`ControlState`]. The rest
/// have defaults and matter only for the gestures that outlive a single
/// element: a drag continues long after the pointer has left the control
/// that started it, so the host has to pump it from wherever it tracks the
/// pointer at the root.
pub trait ControlHost: Sized + 'static {
    fn control_state(&self) -> &ControlState;
    fn control_state_mut(&mut self) -> &mut ControlState;

    /// A slider, split divider or colour pad moved. Both components of `at`
    /// run 0..=1 across the track and are already snapped to its stops;
    /// read `x` for a horizontal control, `y` for a vertical one, both for
    /// a pad.
    ///
    /// Fires on the press as well as on every move, so a click anywhere on
    /// a track jumps there. Implement it once and route on `id`.
    fn track_dragged(&mut self, _id: ComboId, _at: Point<f32>, _cx: &mut Context<Self>) {}

    /// A tab was dropped in a new slot. `from` and `to` are indices into the
    /// bar's items.
    fn tabs_reordered(&mut self, _bar: ComboId, _from: usize, _to: usize, _cx: &mut Context<Self>) {
    }

    /// Forwarded from a surface that blocks the mouse, such as a scrollbar
    /// track. While the pointer is over one, the host sees neither moves nor
    /// releases, so the surface passes them through here.
    fn forwarded_mouse_move(
        &mut self,
        _event: &MouseMoveEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// Forwarded from a surface that blocks the mouse.
    fn forwarded_mouse_up(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// Pumps every in-flight control drag from one pointer position. Call it
/// from the host's mouse-move tracking; it does nothing when nothing is
/// being dragged, so it is safe to call unconditionally.
///
/// Returns whether anything moved, so the host can skip a redraw.
pub fn continue_drags<V: ControlHost>(
    host: &mut V,
    position: Point<Pixels>,
    cx: &mut Context<V>,
) -> bool {
    let mut moved = false;

    if let Some(drag) = &host.control_state().scroll_drag {
        let along = match drag.axis {
            ScrollAxis::Vertical => f32::from(position.y),
            ScrollAxis::Horizontal => f32::from(position.x),
        };
        apply_scroll_drag(drag, along);
        moved = true;
    }
    if let Some((id, at)) = host.control_state().track_ratio_at(position) {
        host.track_dragged(id, at, cx);
        moved = true;
    }
    if host.control_state_mut().drag_tab_to(position) {
        moved = true;
    }
    moved
}

/// Ends every in-flight control drag, applying a finished tab reorder. Call
/// it from the host's mouse-up, and from a mouse-move with no button held.
pub fn end_drags<V: ControlHost>(host: &mut V, cx: &mut Context<V>) {
    if let Some((bar, from, to)) = host.control_state_mut().end_drag() {
        host.tabs_reordered(bar, from, to, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::ControlState;
    use gpui::{Point, px};

    fn at(x: f32, y: f32) -> Point<gpui::Pixels> {
        gpui::point(px(x), px(y))
    }

    /// The target is the whole reason a host opens a menu with one, and it
    /// lives and dies with the open menu. Anything that closes the menu
    /// before running the host's callback hands that callback nothing —
    /// which is exactly what `context_menu` used to do.
    #[test]
    fn a_menu_carries_its_target_until_it_closes() {
        let mut state = ControlState::new();
        assert_eq!(state.menu_target(), None);

        state.open_menu("row", at(10.0, 20.0), "budget.csv");
        assert_eq!(state.menu_target(), Some("budget.csv"));
        assert!(state.is_menu_open("row"));

        state.close_menu();
        assert_eq!(state.menu_target(), None);
        assert_eq!(state.menu_opened_at(), None);
    }

    /// Reopening replaces the target rather than keeping the first one, so a
    /// second right-click on a different row aims at that row.
    #[test]
    fn reopening_a_menu_retargets_it() {
        let mut state = ControlState::new();
        state.open_menu("row", at(0.0, 0.0), "report.pdf");
        state.open_menu("row", at(40.0, 60.0), "budget.csv");
        assert_eq!(state.menu_target(), Some("budget.csv"));
    }

    /// A tween seen for the first time is already where it was asked to be:
    /// nothing slides in from nowhere on the first frame.
    #[test]
    fn a_new_tween_starts_at_its_target() {
        let state = ControlState::new();
        assert_eq!(state.tween("pill", 40.0, super::MOVE), 40.0);
        // And a new target sets off from where it is, not from the end.
        let moving = state.tween("pill", 100.0, super::MOVE);
        assert!((40.0..100.0).contains(&moving), "{moving}");
    }

    /// `tween_from` is the exception, for rows joining a list: it starts at
    /// the given value and heads for the target.
    #[test]
    fn tween_from_starts_where_told() {
        let state = ControlState::new();
        let v = state.tween_from("row", 0.0, 1.0, super::MOVE);
        assert!(v < 0.5, "{v}");
        assert!(state.animating());
    }

    /// Angles go the short way round.
    #[test]
    fn angles_take_the_short_way() {
        let state = ControlState::new();
        state.tween_angle("hue", 350.0, super::MOVE);
        let v = state.tween_angle("hue", 10.0, super::MOVE);
        // Heading up through 360 rather than down through 180.
        assert!(v >= 350.0, "{v}");
    }

    /// A tween made and read in the same instant is at its target, not NaN:
    /// zero elapsed over a zero duration must not be a division.
    #[test]
    fn a_zero_length_tween_is_finished() {
        assert_eq!(
            crate::easing::progress(std::time::Instant::now(), std::time::Duration::ZERO),
            1.0
        );
        let state = ControlState::new();
        for _ in 0..1000 {
            let v = state.tween("fresh", 3.0, super::MOVE);
            assert!(v.is_finite() && v == 3.0, "{v}");
            let s = state.snap("snapped", 4.0);
            assert_eq!(state.tween("snapped", 4.0, super::MOVE), s);
        }
    }

    /// A snapped tween has no motion left in it.
    #[test]
    fn snap_lands_at_once() {
        let state = ControlState::new();
        state.tween("thumb", 0.2, super::MOVE);
        assert_eq!(state.snap("thumb", 0.9), 0.9);
        assert_eq!(state.tween("thumb", 0.9, super::MOVE), 0.9);
        assert!(!state.animating());
    }

    /// A group of rows is "present" from its second frame on, and every row
    /// asking in the same frame gets the same answer.
    #[test]
    fn presence_is_per_frame_and_consistent() {
        let state = ControlState::new();
        assert!(!state.present("list"));
        assert!(!state.present("list"), "second row, same frame");
        state.animating(); // ends the frame
        assert!(state.present("list"));
        // Gone for a couple of frames, and it is new again.
        state.animating();
        state.animating();
        state.animating();
        assert!(!state.present("list"));
    }

    /// Records of things no longer rendered are retired, so a checkbox that
    /// leaves the screen and returns paints its state rather than sliding
    /// from whatever it last showed.
    #[test]
    fn stale_records_are_retired() {
        let state = ControlState::new();
        let id = gpui::ElementId::Name("check".into());
        state.transition(&id, 1, super::SWITCH_SLIDE);
        state.animating();
        state.animating();
        state.animating();
        // First sight again: a different value starts no animation.
        let t = state.transition(&id, 0, super::SWITCH_SLIDE);
        assert_eq!(t, 1.0);
    }

    /// Slow motion stretches every duration, so a slide that would be over
    /// is still under way.
    #[test]
    fn time_scale_stretches_every_duration() {
        let mut state = ControlState::new();
        state.set_time_scale(1000.0);
        state.tween("x", 0.0, std::time::Duration::from_millis(1));
        state.tween("x", 1.0, std::time::Duration::from_millis(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        let v = state.tween("x", 1.0, std::time::Duration::from_millis(1));
        assert!(v < 0.2, "{v}");
        let id = gpui::ElementId::Name("flip".into());
        state.transition(&id, 0, std::time::Duration::from_millis(1));
        state.transition(&id, 1, std::time::Duration::from_millis(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t = state.transition(&id, 1, std::time::Duration::from_millis(1));
        assert!(t < 0.2, "{t}");
    }

    /// `menu_opened_at` identifies one opening, which is how an item can
    /// close the menu it was in without closing one its callback opened.
    #[test]
    fn an_opening_is_identifiable_while_it_lasts() {
        let mut state = ControlState::new();
        state.open_menu("row", at(0.0, 0.0), "report.pdf");
        let opening = state.menu_opened_at().expect("a menu is open");
        assert_eq!(state.menu_opened_at(), Some(opening));

        state.close_menu();
        state.open_menu("row", at(0.0, 0.0), "report.pdf");
        assert!(state.menu_opened_at().is_some());
    }
}
