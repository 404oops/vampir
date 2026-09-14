//! The mutable bits the controls need between frames.
//!
//! Most of a control is a pure function of the data it is handed. What is
//! left over is small and always the same shape: which pop-up is open and
//! how far into its fade it is, what each animated control showed last
//! frame, where things painted, and whatever is being dragged. A host keeps
//! one [`ControlState`], implements [`ControlHost`] for its view, and every
//! control is generic over that host, so nothing here knows the host's type.
//!
//! Everything a control remembers about itself from one frame to the next
//! is one record in one table, filed under a [`Tag`] — a hash, so building
//! one allocates nothing — and retired as soon as the control stops
//! rendering. The table is bounded by what is on screen, not by what has
//! ever been.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, ElementId, Entity, FocusHandle, InteractiveElement, MouseButton,
    MouseMoveEvent, Pixels, Point, ScrollHandle, SharedString, Window,
};

use crate::palette::{Palette, normalized_saturation};
pub use crate::scroll::{ScrollAxis, ScrollDrag, apply_scroll_drag};
use crate::text_input::TextInput;
use crate::theme::{Theme, system_dark};

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

/// Identifies one pop-up, menu, track or tab bar. An element id doubles as
/// its identity, so a host needs no enum of its own. Two of the same kind of
/// widget in one view must not share one.
pub type ComboId = &'static str;

// ---- Tags -------------------------------------------------------------------

/// What a control's record is filed under: its id, the name of the moving
/// part, and — for a row, a tab, a command — which one.
///
/// A tag is the hash of whatever it was built from, so a control can name
/// its parts every frame without allocating: `(id, "pill-x")` is a tuple on
/// the stack, where a formatted string would be two heap allocations per
/// part per frame, freed a moment later because the record already existed.
/// Anything that hashes will do — a `&str`, a `SharedString`, an
/// `ElementId`, a tuple of them — and text hashes as text whichever type
/// it comes in, so `"row"` and `SharedString::from("row")` are one tag.
///
/// Two different names hashing to one tag would make two parts share a
/// record; with sixty-four bits and a screen's worth of parts that is not
/// going to happen, and the worst it could do is a wrong slide.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Tag(u64);

impl Tag {
    /// The tag of anything that hashes. Note that a `Tag` hashes too, so
    /// `Tag::new(tag)` is a different tag — use [`Tag::with`] to extend one.
    pub fn new(of: impl Hash) -> Self {
        let mut hasher = DefaultHasher::new();
        of.hash(&mut hasher);
        Tag(hasher.finish())
    }

    /// A tag under this one: `Tag::new(id).with("shown")` names the same
    /// part as `Tag::new((id, "shown"))` does not — pick one form and keep
    /// to it.
    pub fn with(self, and: impl Hash) -> Self {
        Tag::new((self.0, and))
    }

    pub(crate) fn element_id(self) -> ElementId {
        ("vampir", self.0).into()
    }
}

impl From<&str> for Tag {
    fn from(text: &str) -> Self {
        Tag::new(text)
    }
}

impl From<String> for Tag {
    fn from(text: String) -> Self {
        Tag::new(text.as_str())
    }
}

impl From<SharedString> for Tag {
    fn from(text: SharedString) -> Self {
        Tag::new(text.as_ref())
    }
}

impl From<&SharedString> for Tag {
    fn from(text: &SharedString) -> Self {
        Tag::new(text.as_ref())
    }
}

impl From<ElementId> for Tag {
    fn from(id: ElementId) -> Self {
        Tag::new(id)
    }
}

impl From<&ElementId> for Tag {
    fn from(id: &ElementId) -> Self {
        Tag::new(id)
    }
}

impl<A: Hash, B: Hash> From<(A, B)> for Tag {
    fn from(parts: (A, B)) -> Self {
        Tag::new(parts)
    }
}

impl<A: Hash, B: Hash, C: Hash> From<(A, B, C)> for Tag {
    fn from(parts: (A, B, C)) -> Self {
        Tag::new(parts)
    }
}

/// Tags are hashes already, so the tables keyed by them pass the bits
/// straight through instead of hashing them a second time.
#[derive(Default)]
struct TagHasher(u64);

impl Hasher for TagHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write_u64(&mut self, tag: u64) {
        self.0 = tag;
    }

    fn write(&mut self, bytes: &[u8]) {
        // Never reached for a `Tag`, whose `Hash` is one `write_u64`; kept
        // sound for anything else by folding the bytes in.
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

type TagMap<T> = HashMap<Tag, T, BuildHasherDefault<TagHasher>>;

// ---- Records ----------------------------------------------------------------

/// The kinds of record one tag can hold, one of each at once: a list is
/// `present` as a whole and has a keyboard row, a track has bounds and a
/// tween. The kind is folded into the tag, so the kinds never collide and
/// still share one table.
#[derive(Clone, Copy)]
enum Kind {
    Flip,
    Tween,
    Streak,
    List,
    Bounds,
}

impl Kind {
    fn slot(self, tag: Tag) -> Tag {
        // XOR with a constant is a bijection, so a well-mixed tag stays
        // well-mixed for the table's sake.
        const SALT: [u64; 5] = [
            0x9E37_79B9_7F4A_7C15,
            0xD1B5_4A32_D192_ED03,
            0x8CB9_2BA7_2F3D_8DD7,
            0x5851_F42D_4C95_7F2D,
            0x2545_F491_4F6C_DD1D,
        ];
        Tag(tag.0 ^ SALT[self as usize])
    }
}

/// What one part of one control remembers between frames. Times are
/// microseconds on the state's own clock — see [`ControlState::now`] — and
/// durations microseconds too, which is three words where two `Instant`s
/// and a `Duration` were six.
#[derive(Clone, Copy, Debug)]
enum Record {
    /// A two-state element: the value it showed, and the change under way
    /// since `since` if it has one.
    Flip {
        value: u64,
        since: u64,
        duration: u32,
    },
    /// A continuous value on its way from `from` to `to`.
    Tween {
        from: f32,
        to: f32,
        since: u64,
        duration: u32,
    },
    /// A group of rows: the frame its current run of frames began in.
    Streak { started: u32 },
    /// The keyboard's row in a filtering list, and a hash of the query the
    /// row belongs to.
    List { highlight: usize, query: u64 },
    /// Where something painted last frame.
    Bounds(Bounds<Pixels>),
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// The frame it was last asked about; see [`ControlState::animating`].
    touched: u32,
    record: Record,
}

/// Progress 0..=1 of something that started at `since` and runs for
/// `duration`, both in microseconds. A zero-length change is finished, not
/// undefined: zero over zero would be a NaN, and a NaN offset makes an
/// element vanish for as long as the value is kept.
fn progress_of(now: u64, since: u64, duration: u32) -> f32 {
    if duration == 0 {
        return 1.0;
    }
    (now.saturating_sub(since) as f32 / duration as f32).clamp(0.0, 1.0)
}

fn tween_value(now: u64, from: f32, to: f32, since: u64, duration: u32) -> f32 {
    let t = progress_of(now, since, duration);
    if t >= 1.0 {
        return to;
    }
    from + (to - from) * crate::easing::ease_out_cubic(t)
}

fn micros(duration: Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
}

fn next_dialog_button(indices: &[usize], current: Option<usize>, backward: bool) -> Option<usize> {
    let count = indices.len();
    if count == 0 {
        return None;
    }
    let here = current.and_then(|current| indices.iter().position(|&index| index == current));
    let next = match (here, backward) {
        (Some(here), true) => (here + count - 1) % count,
        (Some(here), false) => (here + 1) % count,
        (None, true) => count - 1,
        (None, false) => 0,
    };
    Some(indices[next])
}

// ---- Overlays and gestures --------------------------------------------------

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
#[derive(Clone, Copy, Debug)]
pub struct TrackDrag {
    pub id: ComboId,
    pub axis: TrackAxis,
    /// Evenly spaced stops to snap to, if the track has any.
    pub stops: Option<u32>,
}

/// A tab being dragged along its bar.
#[derive(Clone, Copy, Debug)]
pub struct TabDrag {
    pub bar: ComboId,
    /// Where the tab started.
    pub from: usize,
    /// Where it would land if released now.
    pub to: usize,
    /// Whether the pointer has travelled far enough from the press to count
    /// as a drag. Until then the gesture is a click on the tab, not a
    /// reorder: a real click almost always wobbles a pixel between press and
    /// release, and that must still select the tab.
    pub moved: bool,
    /// How far into the tab the pointer took hold, in window x, so the tab
    /// rides under the hand where it was grabbed rather than by its edge.
    pub grab: f32,
    /// Where the pointer was pressed, in window x.
    pub pressed: f32,
    /// Where the pointer is now, in window x.
    pub pointer: f32,
}

/// How far a pressed tab has to travel before the press becomes a drag.
pub const TAB_DRAG_THRESHOLD: f32 = 4.0;

/// The one gesture the controls can have in flight. One pointer, one
/// gesture: a thumb, a track and a tab cannot be held at once, so they
/// share one slot rather than three that have to be kept exclusive by hand.
enum Drag {
    Scroll(ScrollDrag),
    Track(TrackDrag),
    Tab(TabDrag),
}

/// A pop-up list that is open, or on its way out.
#[derive(Clone, Copy, Debug)]
pub struct OpenCombo {
    pub id: ComboId,
    pub opened_at: Instant,
    /// The option that was current when the list opened, which is the row
    /// the list lays over the button. Kept through the fade out: by then a
    /// pick has changed the value, and the list must not jump to the new
    /// row on its way out.
    pub opened_on: usize,
    /// Which option the keyboard is on. Not the selection: a list you are
    /// arrowing through has not chosen anything yet, and closing it with
    /// Escape has to leave the old value alone. It commits on Enter and is
    /// dropped when the list closes.
    pub highlight: Option<usize>,
    /// When it was told to close, if it has been. It keeps rendering,
    /// without taking clicks, until the fade finishes.
    pub closing: Option<Instant>,
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

/// A modal dialog that is open, or on its way out.
#[derive(Clone, Copy, Debug)]
pub struct OpenDialog {
    pub id: ComboId,
    pub opened_at: Instant,
    /// When it was told to close, if it has been. It keeps rendering, faded
    /// and without the keyboard, until the exit finishes.
    pub closing: Option<Instant>,
}

/// A command palette that is open.
#[derive(Clone, Debug)]
pub struct OpenPalette {
    pub id: ComboId,
    pub opened_at: Instant,
    /// Where the keyboard was before the palette took it. Closing puts it
    /// back: focus left on a field that no longer exists reaches nothing,
    /// and the shortcut that opens the palette would have to be re-earned
    /// with a click.
    pub return_focus: Option<FocusHandle>,
}

/// The tag the root's focus handle lives under.
const ROOT_FOCUS: &str = "vampir-root";

/// Per-view control state. `Default` is the empty state, which is also the
/// right starting point: nothing open, nothing animating, nothing dragged.
pub struct ControlState {
    /// The hue, saturation and scheme the palette is derived from, for a host with no
    /// theme of its own. See [`crate::theme`].
    pub theme: Theme,

    /// When the state was made. Every time in a record is measured from
    /// here, in microseconds, so a record carries a word rather than an
    /// `Instant`.
    epoch: Instant,
    /// Frames counted by [`ControlState::animating`], which is called once
    /// per render. Wraps, and is only ever compared by difference.
    frame: Cell<u32>,
    /// Every duration is multiplied by this; see
    /// [`ControlState::set_time_scale`]. `None` is one.
    time_scale: Option<f32>,

    /// Everything the controls remember about themselves from frame to
    /// frame: what each two-state element showed, where each continuous
    /// value is on its way to its target, which groups of rows were up last
    /// frame, the keyboard's row in each filtering list, and where each
    /// track, group and slot painted. One table, and every record in it is
    /// retired by [`ControlState::animating`] a frame or two after the thing
    /// it belongs to stops rendering, so the table is the size of the
    /// screen rather than of the session.
    ///
    /// Behind a `RefCell` because the records are read and written during
    /// render, from `&ControlState`: a control notices its own change while
    /// it renders. See [`ControlState::transition`].
    records: RefCell<TagMap<Entry>>,
    color_pads: RefCell<crate::swatch::PadCache>,
    /// Sizes to remember after the thing measured has gone: a disclosure's
    /// body, so it can be shown growing to the height it will have rather
    /// than appearing at it. Written from paint, kept until overwritten,
    /// and bounded by the widgets that ask.
    measured: TagMap<Bounds<Pixels>>,

    /// One focus handle per composite control.
    ///
    /// A group — radio buttons, a segmented control, a tab bar, a tree — is
    /// one stop in the tab order, and the ring belongs on whichever option
    /// inside it is current rather than around the whole group. That means
    /// the handle has to move from option to option as the selection moves,
    /// which it can only do if it outlives all of them: a handle owned by an
    /// option dies the moment that option stops being the current one, and
    /// takes the keyboard with it after a single arrow press. Never retired,
    /// for the same reason: a handle the keyboard is on, or is coming back
    /// to, has to stay the same handle.
    ///
    /// A `RefCell` because this is a cache, not state. A control asks for
    /// its handle while it renders, holding only `&ControlState`, and the
    /// answer is the same handle every frame.
    focus_handles: RefCell<TagMap<FocusHandle>>,
    /// One scroll handle per pop-up list, so a list can read its own offset
    /// and fade its edges. Same reasoning as `focus_handles`.
    scroll_handles: RefCell<TagMap<ScrollHandle>>,

    /// The pop-up list that is open, or still fading out.
    combo: Option<OpenCombo>,
    /// A pop-up just closed by a press outside it, and when. The same
    /// press's click must not reopen it when it landed on that pop-up's own
    /// button. The release that should clear it can land on an occluding
    /// surface and never reach the host, so the marker expires on its own
    /// too.
    combo_dismissed: Option<(ComboId, Instant)>,

    /// The open context menu, if any.
    pub menu: Option<OpenMenu>,
    /// The handle the open context menu holds focus with.
    ///
    /// A menu has to hold focus to hear the arrows, and the handle has to be
    /// the same one every frame — a fresh handle per render would lose focus
    /// the instant it was given. Created once, on the first menu opened, and
    /// reused after that.
    pub menu_focus: Option<FocusHandle>,
    /// Where the keyboard was before the menu took it, so closing puts it
    /// back rather than dropping the reader at the top of the window.
    pub menu_return_focus: Option<FocusHandle>,

    /// The open dialog, if any, or one still fading out.
    pub dialog: Option<OpenDialog>,
    /// The same as `menu_return_focus`, for a modal dialog.
    pub dialog_return_focus: Option<FocusHandle>,
    /// The dialog whose buttons Tab stays among while it is up, and how many
    /// it has; the handles themselves are in `focus_handles` under
    /// [`ControlState::dialog_button_focus`]. Set every frame the dialog
    /// renders and cleared when it does not. See
    /// [`crate::keyboard::move_focus`].
    dialog_trap: RefCell<Option<(ComboId, Vec<usize>)>>,

    /// The open command palette, if any.
    pub palette_overlay: Option<OpenPalette>,
    /// The shortcut recorder waiting for a key chord, if any.
    pub recording: Option<ComboId>,
    pub(crate) recording_keys: Option<[gpui::Subscription; 2]>,
    pub(crate) recording_activation: Option<&'static str>,
    /// True while the mouse is in charge and the focus ring is hidden. Any
    /// mouse press sets it and Tab clears it, and the first Tab after the
    /// mouse only shows where the keyboard is: see `keyboard::move_focus`.
    pub ring_hidden: bool,

    /// The gesture in flight, if any.
    drag: Option<Drag>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlState {
    pub fn new() -> Self {
        Self {
            theme: Theme::default(),
            epoch: Instant::now(),
            frame: Cell::new(0),
            time_scale: None,
            records: RefCell::default(),
            color_pads: RefCell::default(),
            measured: TagMap::default(),
            focus_handles: RefCell::default(),
            scroll_handles: RefCell::default(),
            combo: None,
            combo_dismissed: None,
            menu: None,
            menu_focus: None,
            menu_return_focus: None,
            dialog: None,
            dialog_return_focus: None,
            dialog_trap: RefCell::new(None),
            palette_overlay: None,
            recording: None,
            recording_keys: None,
            recording_activation: None,
            ring_hidden: false,
            drag: None,
        }
    }

    /// Microseconds since the state was made: the clock every record keeps
    /// time by.
    fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    // ---- Pop-ups ----

    /// Whether `combo` is open — not counting one that is fading out.
    pub fn is_combo_open(&self, combo: ComboId) -> bool {
        self.combo
            .as_ref()
            .is_some_and(|open| open.id == combo && open.closing.is_none())
    }

    /// Closes the open pop-up, letting its list fade back out the way it
    /// faded in. A list closed twice leaves the first fade running.
    pub fn close_combo(&mut self) {
        if let Some(open) = self.combo.as_mut()
            && open.closing.is_none()
        {
            open.closing = Some(Instant::now());
            open.highlight = None;
        }
    }

    /// Opens one pop-up on `chosen`, its current option, in place of
    /// whichever was open or fading. The list lays that row over the button,
    /// and the keyboard starts on it, so the first arrow press moves from
    /// there rather than from the top.
    pub fn open_combo(&mut self, combo: ComboId, chosen: usize) {
        self.combo = Some(OpenCombo {
            id: combo,
            opened_at: Instant::now(),
            opened_on: chosen,
            highlight: Some(chosen),
            closing: None,
        });
    }

    /// Puts the keyboard on one option of the open list.
    pub fn highlight_combo(&mut self, index: usize) {
        if let Some(open) = self.combo.as_mut()
            && open.closing.is_none()
        {
            open.highlight = Some(index);
        }
    }

    /// Which option the keyboard is on, falling back to `selected` for a
    /// list opened with the mouse and never arrowed through.
    pub fn combo_highlight_or(&self, selected: usize) -> usize {
        self.combo
            .as_ref()
            .filter(|open| open.closing.is_none())
            .and_then(|open| open.highlight)
            .unwrap_or(selected)
    }

    /// The option `combo`'s list opened on, while the list is on screen at
    /// all — open or fading out.
    pub fn combo_opened_on(&self, combo: ComboId) -> Option<usize> {
        self.combo_fade(combo)?;
        self.combo.as_ref().map(|open| open.opened_on)
    }

    /// `(reveal, still_animating)` for `combo`'s list if it is on screen at
    /// all, open or leaving; `None` once it has gone. Reveal runs 0 to 1,
    /// eased: opening eases out and closing eases in, so both ends of the
    /// motion sit against the button rather than drifting from it.
    pub fn combo_fade(&self, combo: ComboId) -> Option<(f32, bool)> {
        let open = self.combo.as_ref().filter(|open| open.id == combo)?;
        let reveal = self.scaled(COMBO_REVEAL);
        let fade = crate::easing::modal_opacity_at(
            Some(open.opened_at),
            open.closing,
            Instant::now(),
            reveal,
            reveal,
        );
        (open.closing.is_none() || fade.1).then_some(fade)
    }

    /// A press landed outside the open list of `combo`: closes it, and
    /// marks it so the same press's click, if it lands on the list's own
    /// button, does not reopen it.
    pub fn combo_pressed_outside(&mut self, combo: ComboId) {
        self.close_combo();
        self.combo_dismissed = Some((combo, Instant::now()));
    }

    /// The pop-up a press outside it closed a moment ago, if any, clearing
    /// the marker either way: one no toggle consumed, because the click
    /// landed elsewhere, must not eat some later toggle click.
    pub fn take_combo_dismissal(&mut self) -> Option<ComboId> {
        let (combo, at) = self.combo_dismissed.take()?;
        (at.elapsed() < COMBO_REVEAL).then_some(combo)
    }

    // ---- Animation ----

    /// Slows every animation down by `scale` — eight makes a 140ms slide
    /// take over a second. For looking at motion: a screenshot taken during
    /// a slide is the only way to see what the slide actually does, and at
    /// full speed the shutter is slower than the slide.
    /// [`ControlState::slow_motion_from_env`] reads it from the environment.
    pub fn set_time_scale(&mut self, scale: f32) {
        self.time_scale = (scale.is_finite() && scale > 0.0 && scale != 1.0).then_some(scale);
    }

    /// The time scale every animation is running at, when it is not one.
    pub fn time_scale(&self) -> Option<f32> {
        self.time_scale
    }

    /// Reads `VAMPIR_SLOW_MOTION` into the time scale, so a host can be
    /// looked at slowed down without a build: `VAMPIR_SLOW_MOTION=8 app`.
    /// Returns the scale if one was set, for a host that wants to say so on
    /// screen — a screenshot taken in slow motion should not pass for the
    /// real thing.
    pub fn slow_motion_from_env(&mut self) -> Option<f32> {
        let scale = std::env::var("VAMPIR_SLOW_MOTION")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())?;
        self.set_time_scale(scale);
        self.time_scale
    }

    /// A duration at the current time scale.
    pub fn scaled(&self, duration: Duration) -> Duration {
        match self.time_scale {
            Some(scale) => Duration::try_from_secs_f64(duration.as_secs_f64() * f64::from(scale))
                .unwrap_or(Duration::MAX),
            None => duration,
        }
    }

    // ---- Theme ----

    /// Starts following the desktop's colour scheme: reads it now, and keeps
    /// [`Theme::system_is_dark`](crate::theme::Theme::system_is_dark) up to
    /// date as it changes. Call it once, in the view's constructor. A window
    /// that opens dark on a light desktop looks broken before it looks like
    /// a choice.
    pub fn observe_appearance<V: ControlHost>(&mut self, window: &Window, cx: &mut Context<V>) {
        self.theme.set_system_dark(system_dark(window));
        let this = cx.weak_entity();
        let subscription = window.observe_window_appearance(move |window, cx| {
            let dark = system_dark(window);
            this.update(cx, |this, cx| {
                if this.control_state_mut().theme.set_system_dark(dark) {
                    cx.notify();
                }
            })
            .ok();
        });
        self.theme.keep_appearance(subscription);
    }

    /// The palette to hand every control this frame.
    ///
    /// The theme's scheme, hue and saturation are the truth; this is what is shown, and
    /// it follows the truth rather than jumping to it. A scheme crosses over
    /// through [`Palette::mix`] over [`SCHEME_FADE`], so a change from a menu
    /// item, a shortcut, a command palette or the desktop switching to dark
    /// at sunset all arrive the same way: every colour on screen mixed
    /// between the palette it had and the one it is getting. A hue set from
    /// anywhere but its own slider glides the short way round. Saturation
    /// changes also glide; while either slider is held its value sits under
    /// the hand.
    pub fn palette(&self) -> Palette {
        let theme = &self.theme;
        let darkness = self.tween(
            "vampir-scheme-dark",
            if theme.is_dark() { 1.0 } else { 0.0 },
            SCHEME_FADE,
        );
        let held = theme
            .hue_track()
            .is_some_and(|track| self.is_dragging(track));
        let hue = f64::from(if held {
            self.snap("vampir-scheme-hue", theme.hue as f32)
        } else {
            self.tween_angle("vampir-scheme-hue", theme.hue as f32, MOVE)
        });
        let saturation = normalized_saturation(theme.saturation) as f32;
        let saturation_held = theme
            .saturation_track()
            .is_some_and(|track| self.is_dragging(track));
        let saturation = f64::from(if saturation_held {
            self.snap("vampir-scheme-saturation", saturation)
        } else {
            self.tween("vampir-scheme-saturation", saturation, MOVE)
        });
        if darkness <= 0.0 {
            theme.palette_at(hue, saturation, false)
        } else if darkness >= 1.0 {
            theme.palette_at(hue, saturation, true)
        } else {
            Palette::mix(
                theme.palette_at(hue, saturation, false),
                theme.palette_at(hue, saturation, true),
                darkness,
            )
        }
    }

    // ---- Dialogs ----

    /// Opens the modal dialog `id`. It fades in, takes the keyboard when it
    /// first paints, and keeps Tab among its own buttons until it closes.
    pub fn open_dialog(&mut self, id: ComboId) {
        self.dismiss_popups();
        *self.dialog_trap.get_mut() = None;
        self.dialog = Some(OpenDialog {
            id,
            opened_at: Instant::now(),
            closing: None,
        });
    }

    /// Starts the open dialog on its way out. It keeps rendering, faded,
    /// until the exit finishes; a dialog closed twice leaves the first exit
    /// running rather than restarting it.
    pub fn close_dialog(&mut self) {
        *self.dialog_trap.get_mut() = None;
        if let Some(dialog) = self.dialog.as_mut()
            && dialog.closing.is_none()
        {
            dialog.closing = Some(Instant::now());
        }
    }

    /// Whether `id` is open — not counting one that is fading out.
    pub fn is_dialog_open(&self, id: ComboId) -> bool {
        self.dialog
            .as_ref()
            .is_some_and(|dialog| dialog.id == id && dialog.closing.is_none())
    }

    /// `(opacity, still_animating)` for dialog `id` if it is on screen at
    /// all, open or leaving; `None` once it has gone. An exit that started
    /// mid-entrance fades from wherever the entrance had got to.
    pub fn dialog_fade(&self, id: ComboId) -> Option<(f32, bool)> {
        let dialog = self.dialog.as_ref().filter(|dialog| dialog.id == id)?;
        let fade = crate::easing::modal_opacity_at(
            Some(dialog.opened_at),
            dialog.closing,
            Instant::now(),
            self.scaled(crate::easing::MODAL_ENTER),
            self.scaled(crate::easing::MODAL_EXIT),
        );
        (dialog.closing.is_none() || fade.1).then_some(fade)
    }

    // ---- Command palette ----

    /// Opens the command palette `id`, remembering where the keyboard was
    /// and putting it in `query`, emptied. A palette that opens without
    /// focus is a box you have to click before you can type into, which is
    /// the one thing nobody reaching for its shortcut wants to do.
    pub fn open_palette(
        &mut self,
        id: ComboId,
        query: &Entity<TextInput>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.dismiss_popups();
        self.restore_menu_focus(window, cx);
        let return_focus = self
            .palette_overlay
            .take()
            .and_then(|open| open.return_focus)
            .or_else(|| window.focused(cx));
        self.palette_overlay = Some(OpenPalette {
            id,
            opened_at: Instant::now(),
            return_focus,
        });
        self.highlight_list(id, 0);
        query.update(cx, |input, cx| input.set_text("", cx));
        let focus = query.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    }

    /// Closes the open palette and hands the keyboard back where it was, or
    /// to the root — somewhere real, because the query field is about to
    /// stop existing and focus left on it goes nowhere.
    pub fn close_palette(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(open) = self.palette_overlay.take() {
            self.return_focus_to(open.return_focus, window, cx);
        }
    }

    /// Opens the palette if it is closed and closes it if it is open: what
    /// its shortcut does.
    pub fn toggle_palette(
        &mut self,
        id: ComboId,
        query: &Entity<TextInput>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.is_palette_open(id) {
            self.close_palette(window, cx);
        } else {
            self.open_palette(id, query, window, cx);
        }
    }

    pub fn is_palette_open(&self, id: ComboId) -> bool {
        self.palette_overlay
            .as_ref()
            .is_some_and(|open| open.id == id)
    }

    // ---- Filtering lists ----

    /// Which row the keyboard is on in list `list`, given the query the
    /// list is currently filtered by. A changed query puts it back on the
    /// first row: the rows have been re-ranked, and the best match is the
    /// one the person most likely means.
    pub fn list_highlight(&self, list: impl Into<Tag>, query: &str) -> usize {
        let hash = Tag::new(query).0;
        let frame = self.frame.get();
        let mut records = self.records.borrow_mut();
        let entry = records
            .entry(Kind::List.slot(list.into()))
            .or_insert(Entry {
                touched: frame,
                record: Record::List {
                    highlight: 0,
                    query: hash,
                },
            });
        entry.touched = frame;
        let Record::List { highlight, query } = &mut entry.record else {
            return 0;
        };
        if *query != hash {
            *query = hash;
            *highlight = 0;
        }
        *highlight
    }

    /// The keyboard's row in list `list` as last set, whatever the query.
    pub fn list_position(&self, list: impl Into<Tag>) -> usize {
        match self.records.borrow().get(&Kind::List.slot(list.into())) {
            Some(Entry {
                record: Record::List { highlight, .. },
                ..
            }) => *highlight,
            _ => 0,
        }
    }

    /// Puts the keyboard on row `index` of list `list`.
    pub fn highlight_list(&self, list: impl Into<Tag>, index: usize) {
        let frame = self.frame.get();
        let mut records = self.records.borrow_mut();
        let entry = records
            .entry(Kind::List.slot(list.into()))
            .or_insert(Entry {
                touched: frame,
                record: Record::List {
                    highlight: index,
                    query: Tag::new("").0,
                },
            });
        entry.touched = frame;
        if let Record::List { highlight, .. } = &mut entry.record {
            *highlight = index;
        }
    }

    // ---- Records ----

    /// Progress of `tag`'s transition to `value`, 0..=1, starting one when
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
    pub fn transition(&self, tag: impl Into<Tag>, value: u64, duration: Duration) -> f32 {
        let now = self.now();
        let frame = self.frame.get();
        let mut records = self.records.borrow_mut();
        let entry = records.entry(Kind::Flip.slot(tag.into())).or_insert(Entry {
            touched: frame,
            record: Record::Flip {
                value,
                since: now,
                duration: 0,
            },
        });
        entry.touched = frame;
        let Record::Flip {
            value: shown,
            since,
            duration: running,
        } = &mut entry.record
        else {
            return 1.0;
        };
        if *shown != value {
            *shown = value;
            *since = now;
            *running = micros(self.scaled(duration));
        }
        progress_of(now, *since, *running)
    }

    /// Where a two-state element is between its off (0) and on (1) looks,
    /// eased, given whether it is on now. The companion to
    /// [`ControlState::transition`] for the common case: a control mixes its
    /// two looks by this number and gets its slide in either direction, from
    /// wherever it was when the state flipped back.
    ///
    /// Notice it during render, so the host asks [`ControlState::animating`]
    /// after its controls have been built.
    pub fn blend(&self, tag: impl Into<Tag>, on: bool, duration: Duration) -> f32 {
        self.tween(tag, if on { 1.0 } else { 0.0 }, duration)
    }

    /// A continuous value on its way to `target`: the pill under a segmented
    /// control's selection, a row's place in a re-sorted table, the darkness
    /// of a scheme. Returns what to draw this frame.
    ///
    /// Asked for a new target it sets off from wherever it is now, so a
    /// change of mind mid-slide bends the motion rather than restarting it.
    /// The first time a tag is seen it is already at its target: nothing
    /// slides in from nowhere on the first frame; [`ControlState::tween_from`]
    /// is for the one that should. Like [`ControlState::blend`], this is
    /// noticed during render, so the host asks [`ControlState::animating`]
    /// after its controls have been built.
    pub fn tween(&self, tag: impl Into<Tag>, target: f32, duration: Duration) -> f32 {
        self.tween_inner(tag.into(), None, target, duration)
    }

    /// [`ControlState::tween`], but a tag seen for the first time starts at
    /// `initial` and slides to `target` — for a row fading in when it joins
    /// a list. Pair it with [`ControlState::present`] so the first frame of
    /// the whole list is not a hundred rows fading in at once.
    pub fn tween_from(
        &self,
        tag: impl Into<Tag>,
        initial: f32,
        target: f32,
        duration: Duration,
    ) -> f32 {
        self.tween_inner(tag.into(), Some(initial), target, duration)
    }

    /// A tween for an angle in degrees, which goes the short way round: from
    /// 350 to 10 is twenty degrees, not three hundred and forty.
    pub fn tween_angle(&self, tag: impl Into<Tag>, degrees: f32, duration: Duration) -> f32 {
        let tag = tag.into();
        let target = match self.records.borrow().get(&Kind::Tween.slot(tag)) {
            Some(Entry {
                record: Record::Tween { to, .. },
                ..
            }) => {
                let here = *to;
                let delta = (degrees - here).rem_euclid(360.0);
                here + if delta > 180.0 { delta - 360.0 } else { delta }
            }
            _ => degrees,
        };
        self.tween_inner(tag, None, target, duration)
    }

    /// Puts a tween at `value` at once, with no slide. For a value the hand
    /// is dragging: the pointer is the animation, and a control that trails
    /// it reads as lag.
    pub fn snap(&self, tag: impl Into<Tag>, value: f32) -> f32 {
        self.records.borrow_mut().insert(
            Kind::Tween.slot(tag.into()),
            Entry {
                touched: self.frame.get(),
                record: Record::Tween {
                    from: value,
                    to: value,
                    since: self.now(),
                    duration: 0,
                },
            },
        );
        value
    }

    fn tween_inner(&self, tag: Tag, initial: Option<f32>, target: f32, duration: Duration) -> f32 {
        let now = self.now();
        let frame = self.frame.get();
        let duration = micros(self.scaled(duration));
        let mut records = self.records.borrow_mut();
        let entry = records
            .entry(Kind::Tween.slot(tag))
            .or_insert_with(|| Entry {
                touched: frame,
                record: match initial {
                    Some(from) => Record::Tween {
                        from,
                        to: target,
                        since: now,
                        duration,
                    },
                    None => Record::Tween {
                        from: target,
                        to: target,
                        since: now,
                        duration: 0,
                    },
                },
            });
        entry.touched = frame;
        let Record::Tween {
            from,
            to,
            since,
            duration: running,
        } = &mut entry.record
        else {
            return target;
        };
        if *to != target {
            *from = tween_value(now, *from, *to, *since, *running);
            *to = target;
            *since = now;
            *running = duration;
        }
        tween_value(now, *from, *to, *since, *running)
    }

    /// Whether a group of rows — a list, a tree, a menu — was already on
    /// screen last frame. Call it once per row with the *group's* tag.
    ///
    /// A row joining a list that is already up should fade in; a list that
    /// has just appeared as a whole should not fade in row by row, it is
    /// arriving with whatever brought it. This tells the two apart, and it
    /// answers the same for every row in a frame however many ask.
    pub fn present(&self, group: impl Into<Tag>) -> bool {
        let frame = self.frame.get();
        let mut records = self.records.borrow_mut();
        let entry = records
            .entry(Kind::Streak.slot(group.into()))
            .or_insert(Entry {
                touched: frame,
                record: Record::Streak { started: frame },
            });
        let Record::Streak { started } = &mut entry.record else {
            return false;
        };
        // A gap of more than a frame ends the streak.
        if frame.wrapping_sub(entry.touched) > 1 {
            *started = frame;
        }
        entry.touched = frame;
        *started != frame
    }

    /// The scroll handle for a pop-up list, made the first time it is asked
    /// for and the same one thereafter, so the list can read its own offset
    /// and fade the edge that has something past it.
    pub fn scroll(&self, list: impl Into<Tag>) -> ScrollHandle {
        self.scroll_handles
            .borrow_mut()
            .entry(list.into())
            .or_default()
            .clone()
    }

    /// True while a fade or slide still needs frames. A host folds this into
    /// whatever decides to request the next frame.
    ///
    /// Also the end of a frame's bookkeeping: it is called once per render,
    /// after everything has been built, so it counts frames, and retires the
    /// records of anything that has not been rendered for two of them. That
    /// is what lets a row that leaves a list and comes back be new again,
    /// what keeps the table the size of the screen, and why a host that
    /// skips calling this leaks nothing worse than a few stale entries.
    pub fn animating(&self) -> bool {
        let frame = self.frame.get();
        self.frame.set(frame.wrapping_add(1));
        self.color_pads.borrow_mut().retain_recent(frame);
        let now = self.now();
        let mut running = false;
        let mut records = self.records.borrow_mut();
        records.retain(|_, entry| {
            if frame.wrapping_sub(entry.touched) > 1 {
                return false;
            }
            running |= match entry.record {
                Record::Flip {
                    since, duration, ..
                }
                | Record::Tween {
                    since, duration, ..
                } => now.saturating_sub(since) < u64::from(duration),
                _ => false,
            };
            true
        });
        // Retain visits empty buckets too. A large list that disappeared
        // must not leave every later frame scanning its peak allocation.
        if records.capacity() > records.len().saturating_mul(4).max(64) {
            let target = records.len().saturating_mul(2).max(32);
            records.shrink_to(target);
        }
        drop(records);
        if running {
            return true;
        }
        if let Some(open) = &self.combo
            && self.combo_fade(open.id).is_some_and(|(_, running)| running)
        {
            return true;
        }
        let reveal = self.scaled(COMBO_REVEAL);
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.opened_at.elapsed() < reveal)
        {
            return true;
        }
        if self
            .palette_overlay
            .as_ref()
            .is_some_and(|open| open.opened_at.elapsed() < reveal)
        {
            return true;
        }
        self.dialog.as_ref().is_some_and(|dialog| {
            self.dialog_fade(dialog.id)
                .is_some_and(|(_, running)| running)
        })
    }

    // ---- Geometry ----

    pub(crate) fn color_pad_colors(&self, id: ComboId, hue: f64, rows: usize) -> Rc<[gpui::Rgba]> {
        self.color_pads
            .borrow_mut()
            .colors(id, hue, rows, self.frame.get())
    }

    /// Records where something painted this frame, under `tag`. Called from
    /// a paint probe; retired with everything else once the probe stops
    /// painting.
    pub fn record_bounds(&mut self, tag: impl Into<Tag>, bounds: Bounds<Pixels>) {
        self.records.borrow_mut().insert(
            Kind::Bounds.slot(tag.into()),
            Entry {
                touched: self.frame.get(),
                record: Record::Bounds(bounds),
            },
        );
    }

    /// Where `tag` painted last frame, if it did.
    pub fn bounds(&self, tag: impl Into<Tag>) -> Option<Bounds<Pixels>> {
        match self.records.borrow().get(&Kind::Bounds.slot(tag.into())) {
            Some(Entry {
                record: Record::Bounds(bounds),
                ..
            }) => Some(*bounds),
            _ => None,
        }
    }

    /// Records the bounds of a track — a slider's, a pop-up button's, a menu
    /// button's — as it paints. A drag needs them long after the pointer has
    /// left the track, and a list is laid over the button wherever the
    /// button is nested.
    pub fn record_track(&mut self, id: ComboId, bounds: Bounds<Pixels>) {
        self.record_bounds((id, "track"), bounds);
    }

    /// Where track `id` painted last frame.
    pub fn track(&self, id: ComboId) -> Option<Bounds<Pixels>> {
        self.bounds((id, "track"))
    }

    /// Records the bounds of the group around a set of options — a
    /// segmented control's well, a tab bar's — as it paints. The options'
    /// bounds only mean something relative to these.
    pub fn record_group(&mut self, id: ComboId, bounds: Bounds<Pixels>) {
        self.record_bounds((id, "group"), bounds);
    }

    /// Where group `id` painted last frame.
    pub fn group(&self, id: ComboId) -> Option<Bounds<Pixels>> {
        self.bounds((id, "group"))
    }

    /// Records the bounds of option `slot` of group `id` as it paints. The
    /// selection pill slides between these, and a tab drag compares the
    /// pointer with them.
    pub fn record_slot(&mut self, id: ComboId, slot: usize, bounds: Bounds<Pixels>) {
        self.record_bounds((id, "slot", slot), bounds);
    }

    /// Where option `slot` of group `id` painted last frame.
    pub fn slot(&self, id: ComboId, slot: usize) -> Option<Bounds<Pixels>> {
        self.bounds((id, "slot", slot))
    }

    /// Remembers a size after the thing measured has gone: a disclosure's
    /// body, so the next time it opens it can grow to the height it had
    /// rather than appear at it. Unlike [`ControlState::record_bounds`]
    /// this is kept, so tag it by the widget, not by its data.
    pub fn measure(&mut self, tag: impl Into<Tag>, bounds: Bounds<Pixels>) {
        self.measured.insert(tag.into(), bounds);
    }

    /// The last size remembered under `tag`.
    pub fn measured(&self, tag: impl Into<Tag>) -> Option<Bounds<Pixels>> {
        self.measured.get(&tag.into()).copied()
    }

    // ---- Drags ----

    pub fn scroll_dragging(&self) -> bool {
        matches!(self.drag, Some(Drag::Scroll(_)))
    }

    pub fn track_dragging(&self) -> bool {
        matches!(self.drag, Some(Drag::Track(_)))
    }

    /// Whether a particular track is the one being dragged.
    pub fn is_dragging(&self, id: ComboId) -> bool {
        matches!(&self.drag, Some(Drag::Track(drag)) if drag.id == id)
    }

    /// The tab reorder in flight, if any.
    pub fn tab_drag(&self) -> Option<&TabDrag> {
        match &self.drag {
            Some(Drag::Tab(drag)) => Some(drag),
            _ => None,
        }
    }

    fn scroll_drag(&self) -> Option<&ScrollDrag> {
        match &self.drag {
            Some(Drag::Scroll(drag)) => Some(drag),
            _ => None,
        }
    }

    /// Starts a scrollbar thumb drag, in place of any other gesture.
    pub fn begin_scroll_drag(&mut self, drag: ScrollDrag) {
        self.drag = Some(Drag::Scroll(drag));
    }

    /// Starts a drag on a track, recording how it should be read.
    pub fn begin_track_drag(&mut self, id: ComboId, axis: TrackAxis, stops: Option<u32>) {
        self.drag = Some(Drag::Track(TrackDrag { id, axis, stops }));
    }

    /// Starts a tab reorder.
    pub fn begin_tab_drag(&mut self, drag: TabDrag) {
        self.drag = Some(Drag::Tab(drag));
    }

    /// Where along the dragged track a pointer position falls, each
    /// component 0..=1 and already snapped to the track's stops. `None`
    /// when nothing is being dragged, or when the track has not painted yet
    /// and so has no bounds.
    ///
    /// Both components are always filled in; a horizontal track's `y` is
    /// simply not interesting.
    pub fn track_ratio_at(&self, position: Point<Pixels>) -> Option<(ComboId, Point<f32>)> {
        let Some(Drag::Track(drag)) = &self.drag else {
            return None;
        };
        let bounds = self.track(drag.id)?;
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

    /// Which tab in a bar sits under a pointer position, if any.
    pub fn tab_at(&self, bar: ComboId, position: Point<Pixels>) -> Option<usize> {
        (0..)
            .map_while(|slot| self.slot(bar, slot))
            .position(|bounds| bounds.contains(&position))
    }

    /// Updates a tab drag for a new pointer position, returning whether
    /// anything on screen should change: the held tab rides with the pointer,
    /// so every move during a drag is a redraw, and some moves also change
    /// the landing slot.
    ///
    /// Nothing happens until the pointer is [`TAB_DRAG_THRESHOLD`] from
    /// where it pressed: a click that wobbles a pixel stays a click. Past
    /// that, a tab moves only once the pointer is past the midpoint of its
    /// neighbour, so a tab never swaps back and forth under a still hand.
    /// It keeps going while the pointer is past the next neighbour too, so
    /// a fling that clears several tabs in one event lands where the hand
    /// stopped rather than one slot along.
    pub fn drag_tab_to(&mut self, position: Point<Pixels>) -> bool {
        let pointer = f32::from(position.x);
        let Some(Drag::Tab(drag)) = &self.drag else {
            return false;
        };
        if !drag.moved && (pointer - drag.pressed).abs() < TAB_DRAG_THRESHOLD {
            return false;
        }
        let (bar, mut to) = (drag.bar, drag.to);
        // Only walk one way: slots are last frame's geometry, and walking
        // back over ground just covered could otherwise loop for ever.
        let leftwards = self
            .slot(bar, to)
            .is_some_and(|current| pointer < f32::from(current.left()));
        while let Some(current) = self.slot(bar, to) {
            let next = if leftwards {
                if pointer >= f32::from(current.left()) || to == 0 {
                    break;
                }
                to - 1
            } else {
                if pointer <= f32::from(current.right()) {
                    break;
                }
                to + 1
            };
            let Some(neighbour) = self.slot(bar, next) else {
                break;
            };
            let centre = f32::from(neighbour.center().x);
            let past = if leftwards {
                pointer < centre
            } else {
                pointer > centre
            };
            if !past {
                break;
            }
            to = next;
        }
        if let Some(Drag::Tab(drag)) = &mut self.drag {
            drag.pointer = pointer;
            drag.moved = true;
            drag.to = to;
        }
        true
    }

    /// Whether any gesture owned by the controls is in flight. A host with
    /// drags of its own ORs this with them.
    pub fn dragging_anything(&self) -> bool {
        self.drag.is_some()
    }

    /// Ends the drag and clears the dismissal marker. Call it from the
    /// host's mouse-up, and from a mouse-move with no button held: a release
    /// outside the window never arrives, so without that a drag would still
    /// be running when the pointer comes back.
    ///
    /// Returns the finished tab reorder, if the gesture was one and it
    /// actually moved, so the host can apply it.
    pub fn end_drag(&mut self) -> Option<(ComboId, usize, usize)> {
        // A marker no toggle consumed (the click landed elsewhere) must not
        // eat some later toggle click.
        self.combo_dismissed = None;
        match self.drag.take()? {
            Drag::Tab(drag) if drag.moved && drag.from != drag.to => {
                Some((drag.bar, drag.from, drag.to))
            }
            _ => None,
        }
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

    // ---- Focus ----

    /// The focus handle for a composite control, made the first time it is
    /// asked for and the same one thereafter.
    ///
    /// Always a tab stop: a group is exactly one stop however many options
    /// it holds, which is why Tab passes a twenty-tab bar in one press.
    pub fn focus(&self, tag: impl Into<Tag>, cx: &App) -> FocusHandle {
        self.focus_handles
            .borrow_mut()
            .entry(tag.into())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// The root's focus handle: where the keyboard goes when it has nowhere
    /// else to be. [`crate::handle_keys`] puts it on the root element. Not a
    /// tab stop — it is one Tab from the first control, not a control.
    pub fn root_focus(&self, cx: &App) -> FocusHandle {
        self.focus_handles
            .borrow_mut()
            .entry(Tag::from(ROOT_FOCUS))
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Puts the keyboard on the root. For a host about to remove whatever
    /// has focus — switching pages, say: GPUI dispatches nothing from a
    /// handle that is no longer in the tree, not even the root's own
    /// shortcuts, so the keyboard has to be re-homed first.
    pub fn focus_root(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.root_focus(cx), cx);
    }

    /// Hands the keyboard back to `previous`, or to the root if there was
    /// no previous. Anywhere real: an overlay closing takes its focused
    /// element with it, and focus left on that goes nowhere.
    pub fn return_focus_to(
        &self,
        previous: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut App,
    ) {
        match previous {
            Some(handle) => window.focus(&handle, cx),
            None => self.focus_root(window, cx),
        }
    }

    /// The focus handle of button `index` of dialog `id`. One handle per
    /// button, owned here rather than by the buttons, so the dialog can put
    /// the keyboard on its default action when it opens and Tab can find
    /// its way round them while it is up.
    pub fn dialog_button_focus(&self, id: ComboId, index: usize, cx: &App) -> FocusHandle {
        self.focus((id, "button", index), cx)
    }

    /// Records, or clears, the dialog a modal is keeping the keyboard inside
    /// of, and how many buttons it has. Called by
    /// [`crate::containers::dialog`] each frame.
    pub fn set_dialog_trap(&self, trap: Option<(ComboId, usize)>) {
        *self.dialog_trap.borrow_mut() = trap.map(|(id, count)| (id, (0..count).collect()));
    }

    pub(crate) fn set_dialog_buttons(&self, id: ComboId, indices: Vec<usize>) {
        *self.dialog_trap.borrow_mut() = Some((id, indices));
    }

    pub(crate) fn clear_dialog_trap(&self, id: ComboId) {
        let mut trap = self.dialog_trap.borrow_mut();
        if trap.as_ref().is_some_and(|(active, _)| *active == id) {
            *trap = None;
        }
    }

    /// Where Tab goes next while a modal dialog holds the keyboard, or `None`
    /// when none does.
    ///
    /// Skips disabled buttons and keeps focus on the panel when none are
    /// enabled. Focus elsewhere in an open modal returns to its first or
    /// last enabled button.
    pub fn trap_next(&self, window: &Window, backward: bool) -> Option<FocusHandle> {
        let trap = self.dialog_trap.borrow();
        let (id, indices) = trap.as_ref()?;
        if !self.is_dialog_open(id) {
            return None;
        }
        let handles = self.focus_handles.borrow();
        if indices.is_empty() {
            return handles.get(&Tag::new((*id, "panel"))).cloned();
        }
        let button = |index: usize| handles.get(&Tag::new((id, "button", index)));
        let here = indices
            .iter()
            .copied()
            .find(|&index| button(index).is_some_and(|handle| handle.is_focused(window)));
        button(next_dialog_button(indices, here, backward)?).cloned()
    }

    /// Hands the keyboard back to whatever had it before a dialog opened.
    ///
    /// The dialog's own buttons and scrim do this themselves. A host that
    /// closes the dialog some other way — Escape through its own action,
    /// say — calls it, or focus is left on a button that no longer exists
    /// and the next shortcut has nowhere to arrive.
    pub fn restore_dialog_focus(&mut self, window: &mut Window, cx: &mut App) {
        let previous = self.dialog_return_focus.take();
        self.return_focus_to(previous, window, cx);
    }

    /// The focus handle the open menu uses, creating it the first time a
    /// menu is opened in this view.
    pub fn menu_focus(&mut self, cx: &App) -> FocusHandle {
        self.menu_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Returns the keyboard before removing a menu, unless its callback
    /// already moved focus somewhere else.
    pub fn restore_menu_focus(&mut self, window: &mut Window, cx: &mut App) {
        let previous = self.menu_return_focus.take();
        if self
            .menu_focus
            .as_ref()
            .is_some_and(|focus| focus.is_focused(window))
        {
            self.return_focus_to(previous, window, cx);
        }
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

    /// What Escape does when nothing closer to the keyboard has taken it:
    /// closes a pop-up list, a menu or the command palette. Returns whether
    /// there was anything to close, so a host can pass an idle Escape on to
    /// its own overlays.
    pub fn dismiss_overlays(&mut self, window: &mut Window, cx: &mut App) -> bool {
        let had_popup = self
            .combo
            .as_ref()
            .is_some_and(|open| open.closing.is_none())
            || self.menu.is_some();
        self.dismiss_popups();
        self.restore_menu_focus(window, cx);
        if self.palette_overlay.is_some() {
            self.close_palette(window, cx);
            return true;
        }
        had_popup
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
    /// releases, so the surface passes them through here. The default pumps
    /// the toolkit's own drags, which is all a host without drags of its
    /// own needs; a host tracking gestures of its own at the root routes
    /// these into the same place.
    fn forwarded_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if mouse_moved(self, event, cx) {
            cx.notify();
        }
    }

    /// Forwarded from a surface that blocks the mouse.
    fn forwarded_mouse_up(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        end_drags(self, cx);
        cx.notify();
    }
}

/// What a pointer move at the root means to the controls' drags: with a
/// button held it continues them; with none it ends them, because a release
/// outside the window never arrives and a move with no button held is that
/// release. Returns whether anything on screen changed.
///
/// [`handle_mouse`] calls it from the root. A host with drags of its own at
/// the root calls it from its own mouse-move handler instead.
pub fn mouse_moved<V: ControlHost>(
    host: &mut V,
    event: &MouseMoveEvent,
    cx: &mut Context<V>,
) -> bool {
    if !event.dragging() {
        if host.control_state().dragging_anything() {
            end_drags(host, cx);
            return true;
        }
        return false;
    }
    continue_drags(host, event.position, cx)
}

/// Puts the mouse handlers the toolkit's drags need on the host's root: a
/// slider handle, a split divider, a tab or a scrollbar thumb is let go of
/// long after the pointer has left it, so the gesture has to be tracked
/// from the root. Put it on every `.occlude()`d surface too, because an
/// occluding panel swallows the move stream a drag underneath it depends
/// on. [`crate::root`] is this and [`crate::handle_keys`] together.
pub fn handle_mouse<E: InteractiveElement, V: ControlHost>(root: E, cx: &mut Context<V>) -> E {
    root.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
        if mouse_moved(this, event, cx) {
            cx.notify();
        }
    }))
    .on_mouse_up(
        MouseButton::Left,
        cx.listener(|this, _event, _window, cx| {
            end_drags(this, cx);
            cx.notify();
        }),
    )
}

/// Pumps the in-flight control drag from one pointer position. Call it
/// from the host's mouse-move tracking; it does nothing when nothing is
/// being dragged, so it is safe to call unconditionally.
///
/// Returns whether anything moved, so the host can skip a redraw.
pub fn continue_drags<V: ControlHost>(
    host: &mut V,
    position: Point<Pixels>,
    cx: &mut Context<V>,
) -> bool {
    if let Some(drag) = host.control_state().scroll_drag() {
        let along = match drag.axis {
            ScrollAxis::Vertical => f32::from(position.y),
            ScrollAxis::Horizontal => f32::from(position.x),
        };
        apply_scroll_drag(drag, along);
        return true;
    }
    if let Some((id, at)) = host.control_state().track_ratio_at(position) {
        update_track(host, id, at, cx);
        return true;
    }
    host.control_state_mut().drag_tab_to(position)
}

// Presses, keyboard nudges and continuing drags must all reach the same
// binding, including the toolkit-owned theme tracks.
pub(crate) fn update_track<V: ControlHost>(
    host: &mut V,
    id: ComboId,
    at: Point<f32>,
    cx: &mut Context<V>,
) {
    if !host.control_state_mut().theme.apply_track(id, at.x) {
        host.track_dragged(id, at, cx);
    }
}

/// Ends the in-flight control drag, applying a finished tab reorder. Call
/// it from the host's mouse-up, and from a mouse-move with no button held.
pub fn end_drags<V: ControlHost>(host: &mut V, cx: &mut Context<V>) {
    if let Some((bar, from, to)) = host.control_state_mut().end_drag() {
        host.tabs_reordered(bar, from, to, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlState, Entry, Palette, SCHEME_FADE, TabDrag, Tag, TrackAxis};
    use gpui::{Bounds, ElementId, Point, SharedString, px, size};

    fn at(x: f32, y: f32) -> Point<gpui::Pixels> {
        gpui::point(px(x), px(y))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<gpui::Pixels> {
        Bounds::new(at(x, y), size(px(w), px(h)))
    }

    /// The whole point: a record is a few words, not a string and two
    /// timestamps. Sizes are what the refactor bought, so they are pinned.
    #[test]
    fn a_record_is_small() {
        assert_eq!(std::mem::size_of::<Tag>(), 8);
        assert!(
            std::mem::size_of::<Entry>() <= 40,
            "{}",
            std::mem::size_of::<Entry>()
        );
    }

    /// Tags are built from parts without allocating, and the parts matter
    /// in order and in kind.
    #[test]
    fn tags_are_hashes_of_their_parts() {
        assert_eq!(Tag::new(("tabs", "pill-x")), Tag::from(("tabs", "pill-x")));
        assert_ne!(Tag::new(("tabs", "pill-x")), Tag::new(("pill-x", "tabs")));
        assert_ne!(Tag::new(("ab", "c")), Tag::new(("a", "bc")));
        assert_ne!(
            Tag::new(("rows", "shown", 1usize)),
            Tag::new(("rows", "shown", 2usize))
        );
        // Text is text whichever type carries it: a row keyed by a
        // `SharedString` matches the same row keyed by a `&str`.
        let owned: SharedString = String::from("budget.csv").into();
        assert_eq!(
            Tag::new(("tree", "selected", &owned)),
            Tag::new(("tree", "selected", "budget.csv"))
        );
        assert_eq!(Tag::from(&owned), Tag::from("budget.csv"));
        assert_eq!(
            Tag::from(ElementId::Name("x".into())),
            Tag::from(&ElementId::Name("x".into()))
        );
        // Extending a tag is not the same as hashing it again.
        assert_ne!(Tag::new("a").with("b"), Tag::new(Tag::new("a")));
    }

    /// One tag can hold one record of every kind at once, because the kinds
    /// share a table but not a slot: a list is present, has a keyboard row
    /// and painted somewhere, all under its own id.
    #[test]
    fn kinds_do_not_clobber_each_other() {
        let mut state = ControlState::new();
        state.present("list");
        state.highlight_list("list", 3);
        state.record_bounds("list", rect(1.0, 2.0, 3.0, 4.0));
        state.tween("list", 5.0, super::MOVE);
        state.transition("list", 7, super::SWITCH_SLIDE);
        assert_eq!(state.list_position("list"), 3);
        assert_eq!(state.bounds("list"), Some(rect(1.0, 2.0, 3.0, 4.0)));
        assert_eq!(state.tween("list", 5.0, super::MOVE), 5.0);
        assert_eq!(state.transition("list", 7, super::SWITCH_SLIDE), 1.0);
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

    #[test]
    fn reversing_a_blend_starts_from_its_visible_value() {
        let mut state = ControlState::new();
        // Advance the clock instead of sleeping. A long duration keeps
        // scheduler delays negligible next to the motion being tested.
        let duration = std::time::Duration::from_secs(1000);
        assert_eq!(state.blend("switch", false, duration), 0.0);
        assert_eq!(state.blend("switch", true, duration), 0.0);
        state.epoch -= std::time::Duration::from_secs(300);
        let before = state.blend("switch", true, duration);
        assert!((0.6..0.7).contains(&before), "{before}");
        let reversing = state.blend("switch", false, duration);
        assert!(
            (reversing - before).abs() < 0.001,
            "{before} -> {reversing}"
        );
        state.epoch -= std::time::Duration::from_secs(300);
        let returning = state.blend("switch", false, duration);
        assert!(returning < reversing, "{returning} < {reversing}");
        let reversed_again = state.blend("switch", true, duration);
        assert!((reversed_again - returning).abs() < 0.001);
    }

    #[test]
    fn dialog_navigation_skips_disabled_buttons_and_wraps() {
        let enabled = [0, 3, 5];
        assert_eq!(super::next_dialog_button(&enabled, Some(0), false), Some(3));
        assert_eq!(super::next_dialog_button(&enabled, Some(5), false), Some(0));
        assert_eq!(super::next_dialog_button(&enabled, Some(0), true), Some(5));
        assert_eq!(super::next_dialog_button(&enabled, Some(3), true), Some(0));
        // A button can become disabled while it has focus; the next Tab
        // still has to land inside the dialog.
        assert_eq!(super::next_dialog_button(&enabled, Some(2), false), Some(0));
        assert_eq!(super::next_dialog_button(&enabled, None, true), Some(5));
        assert_eq!(super::next_dialog_button(&[], None, false), None);
    }

    #[test]
    fn an_inactive_dialog_cannot_clear_the_active_dialogs_trap() {
        let mut state = ControlState::new();
        state.open_dialog("active");
        state.set_dialog_buttons("active", vec![1, 3]);
        state.clear_dialog_trap("inactive");
        assert_eq!(*state.dialog_trap.borrow(), Some(("active", vec![1, 3])));
        state.close_dialog();
        assert_eq!(*state.dialog_trap.borrow(), None);

        state.set_dialog_trap(Some(("active", 4)));
        state.open_dialog("replacement");
        assert_eq!(*state.dialog_trap.borrow(), None);
    }

    #[test]
    fn invalid_or_extreme_time_scales_cannot_panic() {
        let mut state = ControlState::new();
        for scale in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
            state.set_time_scale(scale);
            assert_eq!(state.time_scale(), None);
            assert_eq!(state.scaled(super::MOVE), super::MOVE);
        }
        state.set_time_scale(f32::MAX);
        assert_eq!(state.scaled(super::MOVE), std::time::Duration::MAX);
        assert_eq!(
            state.scaled(std::time::Duration::ZERO),
            std::time::Duration::ZERO
        );
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
        assert_eq!(super::progress_of(0, 0, 0), 1.0);
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
    /// from whatever it last showed — and the table does not grow with
    /// everything that was ever on screen.
    #[test]
    fn stale_records_are_retired() {
        let state = ControlState::new();
        state.transition("check", 1, super::SWITCH_SLIDE);
        state.transition("check", 0, super::SWITCH_SLIDE);
        assert!(state.animating(), "a change started a slide");
        assert_eq!(state.records.borrow().len(), 1);
        state.animating();
        state.animating();
        assert_eq!(
            state.records.borrow().len(),
            0,
            "two frames unseen, and it is gone"
        );
        // First sight again: a different value starts no animation.
        let t = state.transition("check", 1, super::SWITCH_SLIDE);
        assert_eq!(t, 1.0);
    }

    #[test]
    fn retired_records_release_excess_capacity() {
        let state = ControlState::new();
        for slot in 0..1024 {
            state.snap(("rows", slot), 1.0);
        }
        assert!(state.records.borrow().capacity() >= 1024);
        for _ in 0..3 {
            state.snap(("rows", 0), 1.0);
            state.animating();
        }
        assert_eq!(state.records.borrow().len(), 1);
        assert!(state.records.borrow().capacity() <= 64);
    }

    /// Where things painted is a per-frame record like any other: a tab
    /// closed and gone from the bar takes its width with it, rather than
    /// leaving one behind for every tab the bar ever had.
    #[test]
    fn geometry_is_retired_but_measurements_are_kept() {
        let mut state = ControlState::new();
        state.record_slot("tabs", 0, rect(0.0, 0.0, 80.0, 26.0));
        state.record_bounds(("tabs", "tab", "notes.md"), rect(0.0, 0.0, 80.0, 26.0));
        state.measure(("section", "body"), rect(0.0, 0.0, 300.0, 120.0));
        assert_eq!(
            state.slot("tabs", 0).map(|b| f32::from(b.size.width)),
            Some(80.0)
        );
        assert_eq!(state.tab_at("tabs", at(10.0, 10.0)), Some(0));
        state.animating();
        state.animating();
        state.animating();
        assert_eq!(state.slot("tabs", 0), None);
        assert_eq!(state.bounds(("tabs", "tab", "notes.md")), None);
        assert_eq!(state.tab_at("tabs", at(10.0, 10.0)), None);
        assert_eq!(
            state
                .measured(("section", "body"))
                .map(|b| f32::from(b.size.height)),
            Some(120.0),
            "a disclosure grows to the height it had, however long it was shut"
        );
    }

    /// Slow motion stretches every duration, so a slide that would be over
    /// is still under way.
    ///
    /// The scale is huge on purpose: a millisecond becomes a quarter of an
    /// hour, so a test runner that stalls for a second between two lines,
    /// as a loaded CI machine does, still reads a slide that has barely
    /// begun.
    #[test]
    fn time_scale_stretches_every_duration() {
        let mut state = ControlState::new();
        state.set_time_scale(1_000_000.0);
        state.tween("x", 0.0, std::time::Duration::from_millis(1));
        state.tween("x", 1.0, std::time::Duration::from_millis(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        let v = state.tween("x", 1.0, std::time::Duration::from_millis(1));
        assert!(v < 0.2, "{v}");
        state.transition("flip", 0, std::time::Duration::from_millis(1));
        state.transition("flip", 1, std::time::Duration::from_millis(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t = state.transition("flip", 1, std::time::Duration::from_millis(1));
        assert!(t < 0.2, "{t}");
    }

    /// A dialog is open from the moment it is asked for, fades out rather
    /// than vanishing when closed, and is gone once the fade is over.
    #[test]
    fn a_dialog_fades_out_before_it_is_gone() {
        let mut state = ControlState::new();
        assert_eq!(state.dialog_fade("confirm"), None);
        state.open_dialog("confirm");
        assert!(state.is_dialog_open("confirm"));
        assert!(!state.is_dialog_open("other"));
        let (opacity, running) = state.dialog_fade("confirm").expect("on screen");
        assert!(running && opacity < 1.0, "{opacity}");
        state.close_dialog();
        assert!(!state.is_dialog_open("confirm"), "closing is not open");
        assert!(state.dialog_fade("confirm").is_some(), "still fading");
        assert!(state.animating());
        // A second close does not restart the exit.
        let first = state.dialog.as_ref().and_then(|d| d.closing);
        state.close_dialog();
        assert_eq!(state.dialog.as_ref().and_then(|d| d.closing), first);
        std::thread::sleep(crate::easing::MODAL_EXIT + std::time::Duration::from_millis(20));
        assert_eq!(state.dialog_fade("confirm"), None);
        assert!(!state.animating());
    }

    /// A pop-up list is the same shape as a dialog: open at once, fading
    /// out when closed, gone after the fade — and it keeps the row it opened
    /// on all the way out, because a pick has changed the value by then.
    #[test]
    fn a_combo_fades_out_on_the_row_it_opened_on() {
        let mut state = ControlState::new();
        assert_eq!(state.combo_fade("sort"), None);
        state.open_combo("sort", 2);
        assert!(state.is_combo_open("sort"));
        assert_eq!(
            state.combo_highlight_or(0),
            2,
            "the keyboard starts on the current option"
        );
        let (reveal, running) = state.combo_fade("sort").expect("on screen");
        assert!(running && reveal < 1.0, "{reveal}");
        assert!(state.animating());
        state.highlight_combo(4);
        assert_eq!(state.combo_highlight_or(0), 4);
        state.close_combo();
        assert!(!state.is_combo_open("sort"));
        assert_eq!(
            state.combo_opened_on("sort"),
            Some(2),
            "kept through the fade"
        );
        assert_eq!(
            state.combo_highlight_or(0),
            0,
            "the highlight went with the list"
        );
        assert!(state.combo_fade("sort").is_some(), "still fading");
        std::thread::sleep(super::COMBO_REVEAL + std::time::Duration::from_millis(20));
        assert_eq!(state.combo_fade("sort"), None);
        assert_eq!(state.combo_opened_on("sort"), None);
        assert!(!state.animating());
    }

    /// A press outside a list closes it and marks it, so the same press's
    /// click on the list's own button does not reopen it; the marker is
    /// used once and expires on its own.
    #[test]
    fn a_dismissal_is_taken_once() {
        let mut state = ControlState::new();
        state.open_combo("sort", 0);
        state.combo_pressed_outside("sort");
        assert!(!state.is_combo_open("sort"));
        assert_eq!(state.take_combo_dismissal(), Some("sort"));
        assert_eq!(state.take_combo_dismissal(), None, "taken");
        state.combo_pressed_outside("sort");
        state.end_drag();
        assert_eq!(state.take_combo_dismissal(), None, "a release clears it");
    }

    /// One pointer, one gesture: starting one drag ends another, and only a
    /// tab drag that actually moved comes back as a reorder.
    #[test]
    fn one_gesture_at_a_time() {
        let mut state = ControlState::new();
        state.begin_track_drag("volume", TrackAxis::Horizontal, None);
        assert!(state.is_dragging("volume") && state.track_dragging());
        state.begin_tab_drag(TabDrag {
            bar: "tabs",
            from: 0,
            to: 0,
            moved: false,
            grab: 0.0,
            pressed: 0.0,
            pointer: 0.0,
        });
        assert!(!state.track_dragging(), "the tab took over");
        assert!(state.dragging_anything());
        assert_eq!(
            state.end_drag(),
            None,
            "a press that never moved is a click"
        );
        assert!(!state.dragging_anything());

        state.record_slot("tabs", 0, rect(0.0, 0.0, 80.0, 26.0));
        state.record_slot("tabs", 1, rect(82.0, 0.0, 80.0, 26.0));
        state.begin_tab_drag(TabDrag {
            bar: "tabs",
            from: 0,
            to: 0,
            moved: false,
            grab: 10.0,
            pressed: 10.0,
            pointer: 10.0,
        });
        assert!(
            !state.drag_tab_to(at(11.0, 10.0)),
            "a one-pixel wobble is still a click"
        );
        assert_eq!(state.tab_drag().map(|drag| drag.moved), Some(false));
        assert!(
            state.drag_tab_to(at(40.0, 10.0)),
            "every move during a drag redraws"
        );
        assert_eq!(
            state.tab_drag().map(|drag| drag.to),
            Some(0),
            "not past the neighbour's middle"
        );
        state.drag_tab_to(at(130.0, 10.0));
        assert_eq!(state.tab_drag().map(|drag| drag.to), Some(1));
        assert_eq!(state.end_drag(), Some(("tabs", 0, 1)));
    }

    /// One move event that clears several tabs lands where the pointer is,
    /// not one slot along; and the walk back goes the same way.
    #[test]
    fn a_fling_lands_where_the_pointer_stops() {
        let mut state = ControlState::new();
        for slot in 0..4 {
            let left = slot as f32 * 82.0;
            state.record_slot("tabs", slot, rect(left, 0.0, 80.0, 26.0));
        }
        state.begin_tab_drag(TabDrag {
            bar: "tabs",
            from: 0,
            to: 0,
            moved: false,
            grab: 10.0,
            pressed: 10.0,
            pointer: 10.0,
        });
        // Past the middle of slot 3 (centre 286) in one go.
        state.drag_tab_to(at(300.0, 10.0));
        assert_eq!(state.tab_drag().map(|drag| drag.to), Some(3));
        // Back to just past the middle of slot 1 (centre 122).
        state.drag_tab_to(at(120.0, 10.0));
        assert_eq!(state.tab_drag().map(|drag| drag.to), Some(1));
        // Short of the middle of slot 0 (centre 40): stays.
        state.drag_tab_to(at(50.0, 10.0));
        assert_eq!(state.tab_drag().map(|drag| drag.to), Some(1));
        assert_eq!(state.end_drag(), Some(("tabs", 0, 1)));
    }

    /// A list's keyboard row survives frames while the query is the same,
    /// and goes back to the top when the query changes.
    #[test]
    fn a_new_query_puts_the_keyboard_back_on_the_best_match() {
        let state = ControlState::new();
        assert_eq!(state.list_highlight("results", "re"), 0);
        state.highlight_list("results", 2);
        assert_eq!(state.list_highlight("results", "re"), 2);
        assert_eq!(state.list_position("results"), 2);
        assert_eq!(state.list_highlight("results", "rep"), 0);
        // Another list is another keyboard.
        state.highlight_list("commands", 4);
        assert_eq!(state.list_highlight("results", "rep"), 0);
        assert_eq!(state.list_position("commands"), 4);
    }

    /// The theme's palette follows the truth rather than jumping to it,
    /// and stands still once it has arrived.
    #[test]
    fn the_palette_crosses_over_rather_than_cutting() {
        let mut state = ControlState::new();
        let light = state.palette();
        assert!(!light.is_dark);
        state.theme.scheme = crate::theme::Scheme::Dark;
        // The frame that notices the change paints the old scheme: the
        // crossing has begun, but no time has passed yet.
        assert_eq!(state.palette(), light);
        assert!(state.animating(), "the scheme is crossing over");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let crossing = state.palette();
        assert_ne!(crossing.backdrop, light.backdrop);
        assert_ne!(
            crossing.backdrop,
            Palette::from_hue(state.theme.hue, true).backdrop
        );
        std::thread::sleep(SCHEME_FADE + std::time::Duration::from_millis(20));
        assert_eq!(state.palette(), Palette::from_hue(state.theme.hue, true));
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
