//! What the toolkit's pop-up windows share: where a host window's content
//! sits on the screen.

use gpui::{Pixels, Point, Window, point};

/// Whether a pop-up window may be larger than what it shows, with clear
/// room around it for a shadow. Only AppKit both draws a window's clear
/// pixels clear on every desktop and lets the pointer through them.
/// Elsewhere the room catches the pointer — a hint's would land under the
/// pointer that asked for it and take it away — and an X11 desktop with no
/// compositor paints it black, so there a pop-up window is its panel and
/// nothing more.
pub(crate) const CLEAR_MARGINS: bool = cfg!(target_os = "macos");

/// The screen position of `window`'s top-left content corner, the origin
/// GPUI lays the window out from. [`Window::bounds`] is the whole frame,
/// title bar and all, so a pop-up placed from it beside an element would
/// sit a title bar too high; the chrome is whatever of the frame the
/// viewport does not cover, and it sits above and either side.
pub fn content_origin(window: &Window) -> Point<Pixels> {
    let frame = window.bounds();
    let viewport = window.viewport_size();
    let side = (frame.size.width - viewport.width) / 2.0;
    let top = frame.size.height - viewport.height;
    point(frame.origin.x + side, frame.origin.y + top)
}
