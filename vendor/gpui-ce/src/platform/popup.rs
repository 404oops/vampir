use bitflags::bitflags;
use thiserror::Error;

use crate::{AnyWindowHandle, Bounds, Pixels, Point};

/// Options for a parent-anchored popup window such as a menu, dropdown, context menu or tooltip.
///
/// A popup is placed relative to an anchor rectangle on its parent window rather than at an
/// absolute screen position. The platform resolves the final position, so this works both on
/// systems where the compositor owns window placement (Wayland) and on platforms with absolute
/// coordinates.
///
/// The popup's size comes from [`WindowOptions::window_bounds`](crate::WindowOptions), whose
/// origin is ignored. All coordinates are in logical pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupOptions {
    /// The window the popup is anchored to.
    pub parent: AnyWindowHandle,

    /// The rectangle the popup is positioned relative to, in the parent window's coordinate
    /// space (the same space element bounds are in). For example, a dropdown menu uses the
    /// bounds of the button that opened it.
    pub anchor_rect: Bounds<Pixels>,

    /// Which point of [`Self::anchor_rect`] the popup is anchored to.
    pub anchor: PopupAnchor,

    /// The direction in which the popup extends away from the anchor point. A dropdown that
    /// drops below its button anchors to [`PopupAnchor::BottomLeft`] with a gravity of
    /// [`PopupGravity::BottomRight`] so it grows down and to the right.
    pub gravity: PopupGravity,

    /// How the platform may adjust the popup if the requested placement would put it off-screen.
    pub constraint_adjustment: PopupConstraintAdjustment,

    /// An additional offset applied to the popup after anchoring.
    pub offset: Point<Pixels>,

    /// Whether the popup should take an explicit input grab.
    ///
    /// Grabbing popups behave like menus: they take keyboard focus and are dismissed when the
    /// user clicks outside of them or presses a dismissing key. Use it for menus and comboboxes,
    /// not for tooltips or other passive popups.
    ///
    /// A grab must be requested while the triggering input is still active, in practice the
    /// press of the mouse button that opens the popup. Open grabbing popups from a mouse-down
    /// handler rather than a click handler, otherwise the grab is refused.
    ///
    /// Automatic dismissal only covers input aimed at other applications. A click elsewhere in
    /// your own application still reaches it as usual, so closing the popup in that case is up
    /// to you. Nested grabbing popups must be closed in the reverse order they were opened.
    pub grab: bool,
}

/// The point of the anchor rectangle that a popup is anchored to.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PopupAnchor {
    /// Anchor to the center of the anchor rectangle.
    #[default]
    Center,
    /// Anchor to the center of the top edge.
    Top,
    /// Anchor to the center of the bottom edge.
    Bottom,
    /// Anchor to the center of the left edge.
    Left,
    /// Anchor to the center of the right edge.
    Right,
    /// Anchor to the top-left corner.
    TopLeft,
    /// Anchor to the bottom-left corner.
    BottomLeft,
    /// Anchor to the top-right corner.
    TopRight,
    /// Anchor to the bottom-right corner.
    BottomRight,
}

/// The direction in which a popup extends away from its anchor point.
///
/// For instance, a gravity of [`PopupGravity::BottomRight`] places the popup below and to the
/// right of the anchor point.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PopupGravity {
    /// The popup is centered over the anchor point.
    #[default]
    Center,
    /// The popup extends upwards from the anchor point.
    Top,
    /// The popup extends downwards from the anchor point.
    Bottom,
    /// The popup extends to the left of the anchor point.
    Left,
    /// The popup extends to the right of the anchor point.
    Right,
    /// The popup extends up and to the left of the anchor point.
    TopLeft,
    /// The popup extends down and to the left of the anchor point.
    BottomLeft,
    /// The popup extends up and to the right of the anchor point.
    TopRight,
    /// The popup extends down and to the right of the anchor point.
    BottomRight,
}

bitflags! {
    /// How a popup may be adjusted by the platform if the requested placement would put it
    /// off-screen. If no flags are set, the popup is placed exactly as requested and may be
    /// clipped.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct PopupConstraintAdjustment: u32 {
        /// The popup may be slid horizontally to stay on-screen.
        const SLIDE_X = 1;
        /// The popup may be slid vertically to stay on-screen.
        const SLIDE_Y = 2;
        /// The popup's anchor and gravity may be flipped horizontally to stay on-screen.
        const FLIP_X = 4;
        /// The popup's anchor and gravity may be flipped vertically to stay on-screen.
        const FLIP_Y = 8;
        /// The popup may be shrunk horizontally to stay on-screen.
        const RESIZE_X = 16;
        /// The popup may be shrunk vertically to stay on-screen.
        const RESIZE_Y = 32;
    }
}

/// Returned when the current platform has no native popup implementation yet.
///
/// Native popups are separate from gpui's in-window popovers, which are drawn as elements inside
/// an existing window. A caller that wants a popup on every platform should treat this error as
/// a cue to fall back to that in-window rendering.
#[derive(Debug, Error)]
#[error("popups are not supported on this platform")]
pub struct PopupNotSupportedError;

use crate::{Size, px};

impl PopupAnchor {
    fn point(self, rect: Bounds<Pixels>) -> Point<Pixels> {
        let (left, top) = (rect.origin.x, rect.origin.y);
        let (right, bottom) = (left + rect.size.width, top + rect.size.height);
        let (mid_x, mid_y) = (left + rect.size.width / 2.0, top + rect.size.height / 2.0);
        match self {
            PopupAnchor::Center => Point::new(mid_x, mid_y),
            PopupAnchor::Top => Point::new(mid_x, top),
            PopupAnchor::Bottom => Point::new(mid_x, bottom),
            PopupAnchor::Left => Point::new(left, mid_y),
            PopupAnchor::Right => Point::new(right, mid_y),
            PopupAnchor::TopLeft => Point::new(left, top),
            PopupAnchor::BottomLeft => Point::new(left, bottom),
            PopupAnchor::TopRight => Point::new(right, top),
            PopupAnchor::BottomRight => Point::new(right, bottom),
        }
    }

    fn flipped_x(self) -> Self {
        match self {
            PopupAnchor::Left => PopupAnchor::Right,
            PopupAnchor::Right => PopupAnchor::Left,
            PopupAnchor::TopLeft => PopupAnchor::TopRight,
            PopupAnchor::TopRight => PopupAnchor::TopLeft,
            PopupAnchor::BottomLeft => PopupAnchor::BottomRight,
            PopupAnchor::BottomRight => PopupAnchor::BottomLeft,
            other => other,
        }
    }

    fn flipped_y(self) -> Self {
        match self {
            PopupAnchor::Top => PopupAnchor::Bottom,
            PopupAnchor::Bottom => PopupAnchor::Top,
            PopupAnchor::TopLeft => PopupAnchor::BottomLeft,
            PopupAnchor::BottomLeft => PopupAnchor::TopLeft,
            PopupAnchor::TopRight => PopupAnchor::BottomRight,
            PopupAnchor::BottomRight => PopupAnchor::TopRight,
            other => other,
        }
    }
}

impl PopupGravity {
    /// Where the popup's origin sits relative to the anchor point, as a
    /// fraction of its size on each axis: -1 extends up or left, -0.5
    /// centres, 0 extends down or right.
    fn factors(self) -> (f32, f32) {
        match self {
            PopupGravity::Center => (-0.5, -0.5),
            PopupGravity::Top => (-0.5, -1.0),
            PopupGravity::Bottom => (-0.5, 0.0),
            PopupGravity::Left => (-1.0, -0.5),
            PopupGravity::Right => (0.0, -0.5),
            PopupGravity::TopLeft => (-1.0, -1.0),
            PopupGravity::BottomLeft => (-1.0, 0.0),
            PopupGravity::TopRight => (0.0, -1.0),
            PopupGravity::BottomRight => (0.0, 0.0),
        }
    }

    fn flipped_x(self) -> Self {
        match self {
            PopupGravity::Left => PopupGravity::Right,
            PopupGravity::Right => PopupGravity::Left,
            PopupGravity::TopLeft => PopupGravity::TopRight,
            PopupGravity::TopRight => PopupGravity::TopLeft,
            PopupGravity::BottomLeft => PopupGravity::BottomRight,
            PopupGravity::BottomRight => PopupGravity::BottomLeft,
            other => other,
        }
    }

    fn flipped_y(self) -> Self {
        match self {
            PopupGravity::Top => PopupGravity::Bottom,
            PopupGravity::Bottom => PopupGravity::Top,
            PopupGravity::TopLeft => PopupGravity::BottomLeft,
            PopupGravity::BottomLeft => PopupGravity::TopLeft,
            PopupGravity::TopRight => PopupGravity::BottomRight,
            PopupGravity::BottomRight => PopupGravity::TopRight,
            other => other,
        }
    }
}

/// Where a popup of `size` goes, in the coordinate space of
/// `parent_content_origin` and `work_area`: the anchor and gravity of
/// `options` applied to its anchor rectangle, which is relative to the
/// parent's content area, then its constraint adjustments against the work
/// area — flip first, then slide, then shrink, the order the Wayland
/// positioner uses, so a popup lands in the same place on every backend
/// that resolves placement itself.
///
/// Backends that position popups themselves (macOS, Windows, X11) call
/// this with the parent's content origin and the work area of the display
/// it is on, in whatever logical coordinate space they position windows
/// in, and again whenever the parent moves or the anchor changes.
pub fn place_popup(
    options: &PopupOptions,
    parent_content_origin: Point<Pixels>,
    size: Size<Pixels>,
    work_area: Option<Bounds<Pixels>>,
) -> Bounds<Pixels> {
    let rect = Bounds {
        origin: parent_content_origin + options.anchor_rect.origin,
        size: options.anchor_rect.size,
    };
    let place = |anchor: PopupAnchor, gravity: PopupGravity| {
        let at = anchor.point(rect);
        let (fx, fy) = gravity.factors();
        Point::new(
            at.x + size.width * fx + options.offset.x,
            at.y + size.height * fy + options.offset.y,
        )
    };
    let mut origin = place(options.anchor, options.gravity);
    let mut size = size;
    let Some(area) = work_area else {
        return Bounds { origin, size };
    };
    let adjust = options.constraint_adjustment;
    let (area_left, area_top) = (area.origin.x, area.origin.y);
    let (area_right, area_bottom) = (
        area.origin.x + area.size.width,
        area.origin.y + area.size.height,
    );
    let overflow_x = |x: Pixels| (area_left - x).max(px(0.)) + (x + size.width - area_right).max(px(0.));
    let overflow_y = |y: Pixels| (area_top - y).max(px(0.)) + (y + size.height - area_bottom).max(px(0.));

    if adjust.contains(PopupConstraintAdjustment::FLIP_X) && overflow_x(origin.x) > px(0.) {
        let flipped = place(options.anchor.flipped_x(), options.gravity.flipped_x());
        if overflow_x(flipped.x) < overflow_x(origin.x) {
            origin.x = flipped.x;
        }
    }
    if adjust.contains(PopupConstraintAdjustment::FLIP_Y) && overflow_y(origin.y) > px(0.) {
        let flipped = place(options.anchor.flipped_y(), options.gravity.flipped_y());
        if overflow_y(flipped.y) < overflow_y(origin.y) {
            origin.y = flipped.y;
        }
    }
    if adjust.contains(PopupConstraintAdjustment::SLIDE_X) {
        // Slide the near edge in first, so a popup wider than the area
        // keeps its left edge, where text starts, on screen.
        origin.x = origin.x.min(area_right - size.width).max(area_left);
    }
    if adjust.contains(PopupConstraintAdjustment::SLIDE_Y) {
        origin.y = origin.y.min(area_bottom - size.height).max(area_top);
    }
    if adjust.contains(PopupConstraintAdjustment::RESIZE_X) {
        let left = origin.x.max(area_left);
        size.width = (origin.x + size.width).min(area_right) - left;
        size.width = size.width.max(px(1.));
        origin.x = left;
    }
    if adjust.contains(PopupConstraintAdjustment::RESIZE_Y) {
        let top = origin.y.max(area_top);
        size.height = (origin.y + size.height).min(area_bottom) - top;
        size.height = size.height.max(px(1.));
        origin.y = top;
    }
    Bounds { origin, size }
}
