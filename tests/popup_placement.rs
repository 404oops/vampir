//! The placement every backend that positions native popups itself shares
//! (`gpui::popup::place_popup`, in the vendored GPUI-CE). Here rather than
//! in GPUI-CE's own tests, which the published package cannot build. Only
//! with `command-bar-window`, the feature that depends on the vendored
//! GPUI-CE.

#![cfg(feature = "command-bar-window")]
use gpui::popup::{
    PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions, place_popup,
};
use gpui::{Bounds, Render, WindowHandle, WindowId, point, px, size};

struct Parent;

impl Render for Parent {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

fn options(anchor: PopupAnchor, gravity: PopupGravity) -> PopupOptions {
    PopupOptions {
        parent: WindowHandle::<Parent>::new(WindowId::from(1u64)).into(),
        anchor_rect: Bounds::new(point(px(100.), px(50.)), size(px(200.), px(20.))),
        anchor,
        gravity,
        constraint_adjustment: PopupConstraintAdjustment::empty(),
        offset: point(px(0.), px(0.)),
        grab: false,
    }
}

#[test]
fn anchor_and_gravity_put_the_popup_beside_the_rect_in_screen_space() {
    let popup = size(px(80.), px(40.));
    let at = place_popup(
        &options(PopupAnchor::Bottom, PopupGravity::Bottom),
        point(px(1000.), px(500.)),
        popup,
        None,
    );
    // Centred under the rect's bottom edge: x = 1000 + 100 + 100 - 40.
    assert_eq!(at.origin, point(px(1160.), px(570.)));
    let at = place_popup(
        &options(PopupAnchor::TopLeft, PopupGravity::TopLeft),
        point(px(0.), px(0.)),
        popup,
        None,
    );
    assert_eq!(at.origin, point(px(20.), px(10.)));
}

#[test]
fn offset_moves_the_popup_after_anchoring() {
    let mut below = options(PopupAnchor::Top, PopupGravity::Bottom);
    below.offset = point(px(0.), px(12.));
    let at = place_popup(&below, point(px(0.), px(0.)), size(px(100.), px(40.)), None);
    assert_eq!(at.origin, point(px(150.), px(62.)));
}

#[test]
fn constraints_flip_then_slide_then_shrink_inside_the_work_area() {
    let area = Some(Bounds::new(point(px(0.), px(0.)), size(px(400.), px(120.))));
    let mut below = options(PopupAnchor::Bottom, PopupGravity::Bottom);
    below.constraint_adjustment = PopupConstraintAdjustment::FLIP_Y;
    // The rect spans 70..90. Below it, 90..170 overflows a 120-high area by
    // 50; above it, -10..70 overflows by 10, so the flip wins.
    let parent = point(px(0.), px(20.));
    let at = place_popup(&below, parent, size(px(80.), px(80.)), area);
    assert_eq!(at.origin.y, px(-10.));
    below.constraint_adjustment =
        PopupConstraintAdjustment::FLIP_Y | PopupConstraintAdjustment::SLIDE_Y;
    let at = place_popup(&below, parent, size(px(80.), px(80.)), area);
    assert_eq!(at.origin.y, px(0.));
    let mut wide = options(PopupAnchor::TopLeft, PopupGravity::BottomRight);
    wide.constraint_adjustment =
        PopupConstraintAdjustment::SLIDE_X | PopupConstraintAdjustment::RESIZE_X;
    let at = place_popup(&wide, point(px(0.), px(0.)), size(px(600.), px(10.)), area);
    assert_eq!((at.origin.x, at.size.width), (px(0.), px(400.)));
}

#[test]
fn a_work_area_on_a_second_display_is_honoured_in_its_own_coordinates() {
    // A display to the left of the primary one has negative coordinates.
    let area = Some(Bounds::new(
        point(px(-1440.), px(0.)),
        size(px(1440.), px(900.)),
    ));
    let mut bar = options(PopupAnchor::Top, PopupGravity::Bottom);
    bar.constraint_adjustment = PopupConstraintAdjustment::SLIDE_X;
    let at = place_popup(
        &bar,
        point(px(-1500.), px(100.)),
        size(px(600.), px(40.)),
        area,
    );
    assert_eq!(at.origin.x, px(-1440.));
}
