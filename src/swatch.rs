//! Colour picking: a hue slider, a saturation and lightness pad, and a grid
//! of preset swatches.
//!
//! Values are OKLCH throughout, matching [`crate::Palette`]. Hue is degrees,
//! chroma 0..=[`MAX_CHROMA`], lightness 0..=1. A pad reports positions the
//! same way a slider does, through [`ControlHost::track_dragged`], with `x`
//! for chroma and `y` for lightness.

use gpui::{
    AnyElement, Context, ElementId, KeyDownEvent, MouseButton, MouseDownEvent, Window, div, point,
    prelude::*, px,
};

use crate::color::oklch_to_color;
use crate::controls::{SliderTrack, WidgetContext, slider, track_probe};
use crate::keyboard::{self, Key, Orientation};
use crate::lighting;
use crate::palette::Palette;
use crate::state::{ComboId, ControlHost, MOVE, TrackAxis};

/// Chroma beyond this leaves the sRGB gamut for most hues and lightnesses,
/// so the pad would have a dead region along its right edge.
pub const MAX_CHROMA: f64 = 0.16;

/// An OKLCH colour, as the pickers here pass it around.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Oklch {
    /// Degrees, 0..360.
    pub hue: f64,
    /// 0..=[`MAX_CHROMA`].
    pub chroma: f64,
    /// 0..=1.
    pub lightness: f64,
}

impl Oklch {
    pub fn new(hue: f64, chroma: f64, lightness: f64) -> Self {
        Self {
            hue: hue.rem_euclid(360.0),
            chroma: chroma.clamp(0.0, MAX_CHROMA),
            lightness: lightness.clamp(0.0, 1.0),
        }
    }

    pub fn to_rgba(self) -> gpui::Rgba {
        oklch_to_color(self.lightness, self.chroma, self.hue)
    }
}

/// Hue slider: the same control as [`slider`], labelled for what it does.
///
/// `hue` is in degrees; the drag arrives at
/// [`ControlHost::track_dragged`] as a 0..=1 fraction of the full turn, so
/// multiply by 360.
pub fn hue_slider<V: ControlHost>(
    id: ComboId,
    hue: f64,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    slider(
        id,
        (hue.rem_euclid(360.0) / 360.0) as f32,
        SliderTrack::Continuous,
        ctx,
    )
}

/// Saturation and lightness pad for one hue: chroma left to right, lightness
/// bottom to top, with a ring on the current colour.
///
/// Painted as a grid of cells rather than a true gradient, because gpui has
/// no two-dimensional gradient. At this size the seams do not read, and a
/// pad small enough to matter is small enough to be cheap.
pub fn color_pad<V: ControlHost>(
    id: ComboId,
    color: Oklch,
    height: f32,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;
    const COLUMNS: usize = 40;
    const ROWS: usize = 12;

    let weak = cx.entity().downgrade();
    let state = view.control_state();
    let hue = color.hue;
    let marker_x_key = (color.chroma / MAX_CHROMA) as f32;
    let marker_y_key = 1.0 - color.lightness as f32;
    // Two axes, so the arrows split: left and right move chroma, up and down
    // move lightness. Both report through the same `track_dragged` the
    // pointer uses, because it is the same gesture arriving another way.
    let arrows = cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
        let Some(key) = keyboard::key(event) else {
            return;
        };
        let across = keyboard::nudge(key, Orientation::Horizontal, marker_x_key, 0.025);
        let up = keyboard::nudge(key, Orientation::Vertical, 1.0 - marker_y_key, 0.025);
        let Some(at) = (match (across, up) {
            (Some(x), _) => Some(point(x, marker_y_key)),
            // `nudge` counts up as increasing and the pad paints lightness
            // bottom to top, so the axis is flipped back exactly as the drag
            // does it.
            (_, Some(up)) => Some(point(marker_x_key, 1.0 - up)),
            _ => None,
        }) else {
            return;
        };
        cx.stop_propagation();
        this.track_dragged(id, at, cx);
        cx.notify();
    });

    // Columns of stacked gradients rather than a grid of flat cells.
    // Lightness is the axis the eye catches banding on, so it runs as a
    // gradient inside each cell and the cells meet on shared stops, which
    // leaves it continuous top to bottom. Chroma steps across, where a step
    // of a fortieth of the range is below what anyone can see.
    let mut columns: Vec<AnyElement> = Vec::with_capacity(COLUMNS);
    for column in 0..COLUMNS {
        let chroma = (column as f64 + 0.5) / COLUMNS as f64 * MAX_CHROMA;
        let mut cells: Vec<AnyElement> = Vec::with_capacity(ROWS);
        for row in 0..ROWS {
            // Row 0 is the top, which is the lightest. Oklch lightness is
            // not linear in sRGB, so the range is split into enough bands
            // that interpolating inside one is faithful.
            let top = 1.0 - row as f64 / ROWS as f64;
            let bottom = 1.0 - (row + 1) as f64 / ROWS as f64;
            cells.push(
                div()
                    .flex_1()
                    .w_full()
                    .bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(oklch_to_color(top, chroma, hue), 0.0),
                        gpui::linear_color_stop(oklch_to_color(bottom, chroma, hue), 1.0),
                    ))
                    .into_any_element(),
            );
        }
        columns.push(
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .children(cells)
                .into_any_element(),
        );
    }

    // The marker glides to a colour picked elsewhere — a preset, the hue
    // slider — and sits under the pointer while the pointer has it.
    let (marker_x, marker_y) = if state.is_dragging(id) {
        (
            state.snap((id, "marker-x"), marker_x_key),
            state.snap((id, "marker-y"), marker_y_key),
        )
    } else {
        (
            state.tween((id, "marker-x"), marker_x_key, MOVE),
            state.tween((id, "marker-y"), marker_y_key, MOVE),
        )
    };

    div()
        .id(id)
        .h(px(height))
        .w_full()
        .relative()
        .overflow_hidden()
        .rounded(px(6.0))
        .border_1()
        .border_color(palette.field_border)
        .shadow(lighting::recessed(palette.is_dark))
        .cursor_crosshair()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.control_state_mut()
                    .begin_track_drag(id, TrackAxis::Both, None);
                if let Some((_, at)) = this.control_state().track_ratio_at(event.position) {
                    this.track_dragged(id, at, cx);
                }
                cx.notify();
            }),
        )
        .child(track_probe::<V>(id, weak))
        .child(div().absolute().inset_0().flex().children(columns))
        .child(
            // A white ring with a dark inner edge, so the marker stays
            // visible over both ends of the pad. It is also the current
            // value, so it is where the keyboard lands and where the focus
            // ring belongs.
            div()
                .relative()
                .child(
                    keyboard::ring(ElementId::Name(format!("{id}-marker").into()), 6.0, palette)
                        .on_key_down(arrows),
                )
                .absolute()
                .left(gpui::relative(marker_x))
                .top(gpui::relative(marker_y))
                .ml(px(-6.0))
                .mt(px(-6.0))
                .w(px(12.0))
                .h(px(12.0))
                .rounded_full()
                .border_2()
                .border_color(gpui::white())
                .shadow(vec![lighting::glow(
                    crate::color::rgba(0.0, 0.0, 0.0, 1.0),
                    0.45,
                    2.0,
                )]),
        )
}

/// Grid of preset swatches, with a ring on whichever matches `selected`.
///
/// Presets are how most people actually pick a colour, so put this above the
/// pad rather than beside it.
pub fn swatch_grid<V: ControlHost>(
    id: &'static str,
    swatches: &[Oklch],
    selected: Option<Oklch>,
    size: f32,
    palette: Palette,
    cx: &mut Context<V>,
    on_pick: impl Fn(&mut V, Oklch, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let on_pick = std::rc::Rc::new(on_pick);
    let mut cells: Vec<AnyElement> = Vec::with_capacity(swatches.len());

    for (index, swatch) in swatches.iter().enumerate() {
        let cx: &mut Context<V> = &mut *cx;
        let on_pick = on_pick.clone();
        let key_pick = on_pick.clone();
        let swatch = *swatch;
        // Compared on the values rather than the resulting colour: two
        // OKLCH triples can round to the same sRGB and still be different
        // picks, and the swatch the person clicked is the one to ring.
        let active = selected.is_some_and(|current| {
            (current.hue - swatch.hue).abs() < 0.5
                && (current.chroma - swatch.chroma).abs() < 0.005
                && (current.lightness - swatch.lightness).abs() < 0.005
        });
        cells.push(
            div()
                .id(ElementId::NamedInteger(
                    format!("{id}-swatch").into(),
                    index as u64,
                ))
                .w(px(size))
                .h(px(size))
                .flex_none()
                .rounded(px(5.0))
                .bg(swatch.to_rgba())
                .border_2()
                .border_color(if active {
                    palette.accent
                } else {
                    lighting::rim(swatch.to_rgba(), palette.is_dark)
                })
                .shadow(lighting::raised(palette.is_dark))
                .relative()
                .child(
                    keyboard::ring(
                        ElementId::NamedInteger(format!("{id}-swatch-ring").into(), index as u64),
                        5.0,
                        palette,
                    )
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            if keyboard::key(event) == Some(Key::Activate) {
                                cx.stop_propagation();
                                key_pick(this, swatch, window, cx);
                                cx.notify();
                            }
                        },
                    )),
                )
                .cursor_pointer()
                .on_click(cx.listener(move |this, _event, window, cx| {
                    on_pick(this, swatch, window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        );
    }

    div().flex().flex_wrap().gap(px(6.0)).children(cells)
}

/// Twelve evenly spaced hues at one lightness and chroma: a reasonable
/// default row for [`swatch_grid`].
pub fn hue_wheel(lightness: f64, chroma: f64) -> Vec<Oklch> {
    (0..12)
        .map(|step| Oklch::new(step as f64 * 30.0, chroma, lightness))
        .collect()
}
