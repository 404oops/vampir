//! Scrolling: overlay-scrollbar geometry, thumb dragging, and the fade that
//! keeps a scroll edge from slicing whatever is passing it.
//!
//! Split out from the drawing so a host can keep a drag alive from its own
//! root mouse handlers: the pointer leaves the thin track almost at once,
//! and every move after that arrives somewhere else entirely.

use gpui::{
    AnyElement, Rgba, ScrollHandle, div, linear_color_stop, linear_gradient, point, prelude::*, px,
};

use crate::state::{ControlState, SWITCH_SLIDE};

/// Shortest a thumb is allowed to get, however long the content is.
pub const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// Width of the vertical track, height of the horizontal one.
pub const SCROLLBAR_THICKNESS: f32 = 10.0;
/// The visible bar inside that track.
pub const THUMB_THICKNESS: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

impl ScrollAxis {
    pub fn other(self) -> Self {
        match self {
            ScrollAxis::Vertical => ScrollAxis::Horizontal,
            ScrollAxis::Horizontal => ScrollAxis::Vertical,
        }
    }
}

/// A thumb drag in flight. The host stores it and feeds pointer positions to
/// [`apply_scroll_drag`] until the button comes up.
pub struct ScrollDrag {
    pub handle: ScrollHandle,
    pub axis: ScrollAxis,
    /// Where inside the thumb the pointer grabbed it, in px. Without this
    /// the thumb would jump so its start sat under the cursor.
    pub grab: f32,
}

pub struct ScrollbarGeometry {
    pub track_start: f32,
    /// Usable track: the viewport minus the corner reserved for the other
    /// axis's bar.
    pub track_len: f32,
    pub track_end_inset: f32,
    pub thumb_len: f32,
    pub thumb_pos: f32,
    pub max_offset: f32,
}

/// (track start, viewport length, max offset, current offset) along `axis`.
fn axis_metrics(handle: &ScrollHandle, axis: ScrollAxis) -> (f32, f32, f32, f32) {
    let bounds = handle.bounds();
    match axis {
        ScrollAxis::Vertical => (
            f32::from(bounds.top()),
            f32::from(bounds.size.height),
            f32::from(handle.max_offset().y),
            f32::from(handle.offset().y),
        ),
        ScrollAxis::Horizontal => (
            f32::from(bounds.left()),
            f32::from(bounds.size.width),
            f32::from(handle.max_offset().x),
            f32::from(handle.offset().x),
        ),
    }
}

fn axis_scrollable(handle: &ScrollHandle, axis: ScrollAxis) -> bool {
    let (_, viewport_len, max_offset, _) = axis_metrics(handle, axis);
    max_offset > 0.5 && viewport_len > 0.0
}

/// `None` when the content fits and no bar should be drawn.
pub fn scrollbar_geometry(handle: &ScrollHandle, axis: ScrollAxis) -> Option<ScrollbarGeometry> {
    let (track_start, viewport_len, max_offset, offset) = axis_metrics(handle, axis);
    if max_offset <= 0.5 || viewport_len <= 0.0 {
        return None;
    }
    // When both bars are on screen, stop each one short of the shared corner.
    // Otherwise the two tracks overlap there and whichever is painted last
    // swallows the other's drags.
    let track_end_inset = if axis_scrollable(handle, axis.other()) {
        SCROLLBAR_THICKNESS
    } else {
        0.0
    };
    let track_len = (viewport_len - track_end_inset).max(1.0);
    let content_len = viewport_len + max_offset;
    let thumb_len = (track_len * viewport_len / content_len)
        .max(SCROLLBAR_MIN_THUMB)
        .min(track_len);
    let usable = (track_len - thumb_len).max(0.0);
    let fraction = (-offset / max_offset).clamp(0.0, 1.0);
    Some(ScrollbarGeometry {
        track_start,
        track_len,
        track_end_inset,
        thumb_len,
        thumb_pos: fraction * usable,
        max_offset,
    })
}

/// Scrolls the handle to match a pointer position, in window coordinates
/// along the drag's axis.
pub fn apply_scroll_drag(drag: &ScrollDrag, position: f32) {
    let Some(geometry) = scrollbar_geometry(&drag.handle, drag.axis) else {
        return;
    };
    let usable = (geometry.track_len - geometry.thumb_len).max(1.0);
    let thumb_pos = (position - geometry.track_start - drag.grab).clamp(0.0, usable);
    let fraction = thumb_pos / usable;
    let target = -fraction * geometry.max_offset;
    let current = drag.handle.offset();
    match drag.axis {
        ScrollAxis::Vertical => drag.handle.set_offset(point(current.x, px(target))),
        ScrollAxis::Horizontal => drag.handle.set_offset(point(px(target), current.y)),
    }
}

/// How far a scroll-edge fade reaches.
pub const SCROLL_FADE: f32 = 28.0;

/// Fades content out at the edges of a scrolling area rather than slicing it.
///
/// A scroll container clips, and a clip cuts through whatever happens to be
/// at the boundary — mid-word, mid-glyph, half a row. That reads as damage
/// rather than as more to come, which is the one thing an edge is there to
/// say. These are gradients from the surface colour to nothing, drawn over
/// the edges of the viewport, and only on an edge that has something past it.
///
/// `start` and `end` are the colours behind the content at each edge — the
/// same colour twice for a flat surface, two for a lit one. The fade has to
/// land on the surface exactly, or the gradient shows up as a band of its
/// own; [`crate::lighting::lit_stops`] gives the two ends of a lit surface
/// to interpolate between.
///
/// Each fade eases in as its edge gains something to hide and eases out as
/// it runs out, through `state`, keyed by `id`; a veil that snapped on the
/// moment the first pixel scrolled would itself be the flash it exists to
/// prevent.
///
/// Put them in the `.relative()` wrapper around the scroll container, after
/// it so they paint over the content:
///
/// ```ignore
/// div()
///     .relative()
///     .child(div().id("page").overflow_y_scroll().track_scroll(&scroll).child(body))
///     .children(scroll_fades(&self.controls, "page", &scroll, ScrollAxis::Vertical, backdrop, backdrop))
/// ```
pub fn scroll_fades(
    state: &ControlState,
    id: &str,
    handle: &ScrollHandle,
    axis: ScrollAxis,
    start: Rgba,
    end: Rgba,
) -> Vec<AnyElement> {
    let (offset, max) = match axis {
        ScrollAxis::Vertical => (
            f32::from(handle.offset().y),
            f32::from(handle.max_offset().y),
        ),
        ScrollAxis::Horizontal => (
            f32::from(handle.offset().x),
            f32::from(handle.max_offset().x),
        ),
    };
    // Offsets run from 0 at the start to -max at the end, so a fade belongs
    // on the start edge once the offset has left zero, and on the end edge
    // until it arrives at -max. Content that fits wants neither.
    let scrollable = max > 0.5;
    let wanted = |shown: bool| if scrollable && shown { 1.0 } else { 0.0 };
    let at_start = state.tween((id, "fade-start"), wanted(offset < -0.5), SWITCH_SLIDE);
    let at_end = state.tween((id, "fade-end"), wanted(offset > -max + 0.5), SWITCH_SLIDE);
    let mut fades = Vec::new();
    if at_start > 0.01 {
        fades.push(edge_fade(axis, true, start, at_start));
    }
    if at_end > 0.01 {
        fades.push(edge_fade(axis, false, end, at_end));
    }
    fades
}

fn edge_fade(axis: ScrollAxis, at_start: bool, surface: Rgba, opacity: f32) -> AnyElement {
    let clear = crate::color::with_alpha(surface, 0.0);
    // Solid against the edge it guards, clear where the content is legible.
    let (near, far) = if at_start {
        (surface, clear)
    } else {
        (clear, surface)
    };
    let angle = match axis {
        ScrollAxis::Vertical => 180.0,
        ScrollAxis::Horizontal => 90.0,
    };
    let background = linear_gradient(
        angle,
        linear_color_stop(near, 0.0),
        linear_color_stop(far, 1.0),
    );

    // No listeners and no `occlude`, so the content underneath still takes
    // the mouse: this is a veil, not a lid.
    let fade = div().absolute().opacity(opacity).bg(background);
    match (axis, at_start) {
        (ScrollAxis::Vertical, true) => fade.top_0().left_0().right_0().h(px(SCROLL_FADE)),
        (ScrollAxis::Vertical, false) => fade.bottom_0().left_0().right_0().h(px(SCROLL_FADE)),
        (ScrollAxis::Horizontal, true) => fade.left_0().top_0().bottom_0().w(px(SCROLL_FADE)),
        (ScrollAxis::Horizontal, false) => fade.right_0().top_0().bottom_0().w(px(SCROLL_FADE)),
    }
    .into_any_element()
}
