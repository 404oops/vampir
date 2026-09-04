//! Easing curves, and the fade helper the pop-up panels share.
//!
//! Everything here is driven by an [`Instant`] rather than a frame counter,
//! so a transition ends on its own and the host stops asking for frames
//! instead of pinning the display link.

use std::time::{Duration, Instant};

/// Fast start, decelerating into the end value. The default for anything
/// arriving: a pop-up opening, a switch settling.
pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Slow start, accelerating away. The default for anything leaving, so both
/// ends of a motion sit against the control rather than drifting from it.
pub fn ease_in_cubic(t: f32) -> f32 {
    t * t * t
}

pub fn lerp_f32(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// Progress 0..=1 of a transition that started at `since` and runs for
/// `duration`.
pub fn progress(since: Instant, duration: Duration) -> f32 {
    // A zero-length transition is finished, not undefined: read in the same
    // instant it was made, zero over zero is NaN, and a NaN offset makes an
    // element vanish for as long as the value is kept.
    if duration.is_zero() {
        return 1.0;
    }
    (since.elapsed().as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// A modal panel takes this long to fade in, and rather less to leave: an
/// exit that matches its entrance feels slow, because nobody is waiting to
/// read it any more.
pub const MODAL_ENTER: Duration = Duration::from_millis(220);
pub const MODAL_EXIT: Duration = Duration::from_millis(160);

/// `(opacity, still_animating)` for a panel that faded in at `enter_at` and
/// may now be leaving since `exit_at`. An exit overrides an entrance, so
/// dismissing a panel mid-open fades it from wherever it got to.
pub fn modal_opacity(enter_at: Option<Instant>, exit_at: Option<Instant>) -> (f32, bool) {
    if let Some(since) = exit_at {
        let t = progress(since, MODAL_EXIT);
        (1.0 - ease_in_cubic(t), t < 1.0)
    } else if let Some(since) = enter_at {
        let t = progress(since, MODAL_ENTER);
        (ease_out_cubic(t), t < 1.0)
    } else {
        (1.0, false)
    }
}
