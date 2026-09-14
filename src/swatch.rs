//! Colour picking: a hue slider, a saturation and lightness pad, and a grid
//! of preset swatches.
//!
//! Values are OKLCH throughout, matching [`crate::Palette`]. Hue is degrees,
//! chroma 0..=[`MAX_CHROMA`], lightness 0..=1. A pad reports positions the
//! same way a slider does, through [`ControlHost::track_dragged`], with `x`
//! for chroma and `y` for lightness.

use gpui::{
    AnyElement, Bounds, Context, ElementId, KeyDownEvent, MouseButton, MouseDownEvent, Pixels,
    Rgba, Window, canvas, div, fill, point, prelude::*, px,
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

/// How many chroma steps the pad paints across. A step of a fortieth of
/// the range is below what anyone can see.
const PAD_COLUMNS: usize = 40;

/// How many lightness steps a pad of `height` points paints: one per two
/// points, so a step is under a hundredth of lightness at any size a pad is
/// drawn at. Lightness is the axis the eye catches banding on, so it gets
/// the finer grid.
pub(crate) fn pad_rows(height: f32) -> usize {
    ((height / 2.0).ceil().max(1.0) as usize).min(120)
}

/// Every cell of the pad: where it sits and what fills it. Column 0 is the
/// least chroma and row 0 the most lightness, so the pad reads chroma left
/// to right and lightness bottom to top. Neighbours share their edges
/// exactly — the same expression on both sides — so snapping to device
/// pixels can never open a seam between them.
pub(crate) fn pad_cells(
    bounds: Bounds<Pixels>,
    hue: f64,
    columns: usize,
    rows: usize,
) -> Vec<(Bounds<Pixels>, Rgba)> {
    let xs: Vec<Pixels> = (0..=columns)
        .map(|column| bounds.origin.x + bounds.size.width * (column as f32 / columns as f32))
        .collect();
    let ys: Vec<Pixels> = (0..=rows)
        .map(|row| bounds.origin.y + bounds.size.height * (row as f32 / rows as f32))
        .collect();
    let mut cells = Vec::with_capacity(columns * rows);
    for column in 0..columns {
        let chroma = (column as f64 + 0.5) / columns as f64 * MAX_CHROMA;
        for row in 0..rows {
            let lightness = 1.0 - (row as f64 + 0.5) / rows as f64;
            cells.push((
                Bounds::from_corners(
                    point(xs[column], ys[row]),
                    point(xs[column + 1], ys[row + 1]),
                ),
                oklch_to_color(lightness, chroma, hue),
            ));
        }
    }
    cells
}

/// Saturation and lightness pad for one hue: chroma left to right, lightness
/// bottom to top, with a ring on the current colour.
///
/// Painted as a grid of flat cells straight into the scene, because gpui has
/// no two-dimensional gradient. Flat cells rather than gradient ones on
/// purpose: gpui mixes gradients in Oklab on the GPU, and between two
/// gamut-clipped stops that mix leaves the gamut and comes back as `NaN`,
/// which paints black. Straight into the scene rather than as elements
/// because a few thousand cells is nothing to the GPU and far too much for
/// layout to do again on every pointer move.
pub fn color_pad<V: ControlHost>(
    id: ComboId,
    color: Oklch,
    height: f32,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let WidgetContext { palette, view, cx } = ctx;

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

    // The cells are sized from the bounds the canvas is actually given, so
    // the grid follows the pad rather than the height it asked for.
    let field = canvas(
        |_bounds, _window, _cx| (),
        move |bounds, _state, window, _cx| {
            let rows = pad_rows(f32::from(bounds.size.height));
            for (cell, fill_color) in pad_cells(bounds, hue, PAD_COLUMNS, rows) {
                window.paint_quad(fill(cell, fill_color));
            }
        },
    )
    .absolute()
    .inset_0();

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
        .child(field)
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

#[cfg(test)]
mod tests {
    use super::{PAD_COLUMNS, pad_cells, pad_rows};
    use crate::color::{channels, relative_luminance};
    use gpui::{Bounds, point, px, size};

    fn pad() -> Bounds<gpui::Pixels> {
        Bounds::new(point(px(10.0), px(20.0)), size(px(400.0), px(140.0)))
    }

    #[test]
    fn cells_tile_the_pad_without_gaps_or_overlap() {
        let bounds = pad();
        let (columns, rows) = (PAD_COLUMNS, pad_rows(140.0));
        let cells = pad_cells(bounds, 200.0, columns, rows);
        assert_eq!(cells.len(), columns * rows);
        for column in 0..columns {
            for row in 0..rows {
                let (cell, _) = cells[column * rows + row];
                if row + 1 < rows {
                    let (below, _) = cells[column * rows + row + 1];
                    assert_eq!(cell.bottom(), below.top());
                }
                if column + 1 < columns {
                    let (right, _) = cells[(column + 1) * rows + row];
                    assert_eq!(cell.right(), right.left());
                }
            }
        }
        let (first, _) = cells[0];
        let (last, _) = cells[cells.len() - 1];
        assert_eq!(first.origin, bounds.origin);
        assert_eq!(last.bottom_right(), bounds.bottom_right());
    }

    #[test]
    fn lightness_runs_bottom_to_top_and_chroma_left_to_right() {
        let rows = pad_rows(140.0);
        let cells = pad_cells(pad(), 30.0, PAD_COLUMNS, rows);
        let luminance =
            |column: usize, row: usize| relative_luminance(cells[column * rows + row].1);
        assert!(luminance(0, 0) > luminance(0, rows / 2));
        assert!(luminance(0, rows / 2) > luminance(0, rows - 1));
        // The leftmost column is a near grey, the rightmost is the hue in
        // full: the spread of its channels is what chroma looks like in sRGB.
        let spread = |column: usize| {
            let [r, g, b, _] = channels(cells[column * rows + rows / 2].1);
            r.max(g).max(b) - r.min(g).min(b)
        };
        assert!(spread(0) < 0.05);
        assert!(spread(PAD_COLUMNS - 1) > 0.3);
    }

    #[test]
    fn every_cell_is_a_displayable_colour() {
        for hue in [0.0, 97.0, 180.0, 264.0, 350.0] {
            for (_, color) in pad_cells(pad(), hue, PAD_COLUMNS, pad_rows(140.0)) {
                for channel in channels(color) {
                    assert!((0.0..=1.0).contains(&channel), "hue {hue}: {channel}");
                }
            }
        }
    }

    #[test]
    fn rows_follow_the_height_within_reason() {
        assert_eq!(pad_rows(0.0), 1);
        assert_eq!(pad_rows(140.0), 70);
        assert_eq!(pad_rows(141.0), 71);
        assert_eq!(pad_rows(2000.0), 120);
    }
}
