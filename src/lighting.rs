//! Lighting: the few gradient, highlight and shadow recipes that give
//! surfaces and controls a lit, physical feel.
//!
//! Every recipe takes the colour it is lighting plus a light/dark flag and
//! derives the rest, so a palette that changes hue or scheme keeps working
//! with no second set of constants to maintain.

use gpui::{Background, BoxShadow, Hsla, Rgba, linear_color_stop, linear_gradient, point, px};

use crate::color::{channels, rgba, to_hsla};

/// Mixes `color` toward white (`amount` > 0) or black (`amount` < 0).
pub fn shade(color: Rgba, amount: f32) -> Rgba {
    let target = if amount >= 0.0 { 1.0 } else { 0.0 };
    let t = amount.abs().clamp(0.0, 1.0);
    let [r, g, b, a] = channels(color);
    rgba(
        r + (target - r) * t,
        g + (target - g) * t,
        b + (target - b) * t,
        a,
    )
}

fn white(alpha: f32) -> Hsla {
    to_hsla(rgba(1.0, 1.0, 1.0, alpha))
}

fn black(alpha: f32) -> Hsla {
    to_hsla(rgba(0.0, 0.0, 0.0, alpha))
}

/// Perceived brightness of a colour, 0..=1.
fn luminance(color: Rgba) -> f32 {
    let [r, g, b, _] = channels(color);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Vertical gradient lit from above: `lift` lighter at the top and darker
/// at the bottom. Near-white fills cannot get any lighter, so the brighter
/// the base the more of the effect goes into darkening the bottom edge;
/// that keeps light mode as sculpted as dark mode.
pub fn lit(base: Rgba, lift: f32) -> Background {
    let (top, bottom) = lit_stops(base, lift);
    linear_gradient(
        180.0,
        linear_color_stop(top, 0.0),
        linear_color_stop(bottom, 1.0),
    )
}

/// [`lit`] at a fraction of its opacity, for a highlight washing in or out
/// over whatever is behind it: a selected row arriving, a keyboard
/// highlight moving on.
pub fn lit_at(base: Rgba, lift: f32, opacity: f32) -> Background {
    let (top, bottom) = lit_stops(base, lift);
    linear_gradient(
        180.0,
        linear_color_stop(crate::color::with_alpha(top, opacity), 0.0),
        linear_color_stop(crate::color::with_alpha(bottom, opacity), 1.0),
    )
}

/// A flat surface crossing over into a lit one, `on` of the way there: a
/// checkbox's well becoming its filled control, a chip taking the accent.
/// Both ends of the gradient are mixed, so at `on == 1.0` this is exactly
/// [`lit`] and at `0.0` exactly the flat colour.
pub fn lit_mix(flat: Rgba, base: Rgba, lift: f32, on: f32) -> Background {
    let (top, bottom) = lit_stops(base, lift);
    linear_gradient(
        180.0,
        linear_color_stop(crate::color::lerp(flat, top, on), 0.0),
        linear_color_stop(crate::color::lerp(flat, bottom, on), 1.0),
    )
}

/// The colours [`lit`] runs between, top then bottom.
///
/// For anything that has to land exactly on a lit surface — a scroll-edge
/// fade, say — rather than guess at it. Interpolate between the two by how
/// far down the surface the point sits.
pub fn lit_stops(base: Rgba, lift: f32) -> (Rgba, Rgba) {
    let bottom = lift * (0.5 + 0.5 * luminance(base));
    (shade(base, lift), shade(base, -bottom))
}

/// Rim colour for a raised control: a touch darker than its fill.
pub fn rim(fill: Rgba, dark: bool) -> Rgba {
    shade(fill, if dark { -0.35 } else { -0.2 })
}

/// Raised control: a crisp highlight along the inner top edge, a soft drop
/// shadow underneath and, in light mode, a faint bevel along the inner
/// bottom edge (the highlight alone is invisible on near-white fills).
pub fn raised(dark: bool) -> Vec<BoxShadow> {
    let mut shadows = vec![
        BoxShadow {
            color: white(if dark { 0.08 } else { 0.55 }),
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(0.0),
            spread_radius: px(0.0),
            inset: true,
        },
        BoxShadow {
            color: black(if dark { 0.22 } else { 0.1 }),
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(2.0),
            spread_radius: px(0.0),
            inset: false,
        },
    ];
    if !dark {
        shadows.push(BoxShadow {
            color: black(0.05),
            offset: point(px(0.0), px(-1.0)),
            blur_radius: px(0.0),
            spread_radius: px(0.0),
            inset: true,
        });
    }
    shadows
}

/// Recessed well (fields, tracks): a soft shadow inside the top edge.
pub fn recessed(dark: bool) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: black(if dark { 0.3 } else { 0.1 }),
        offset: point(px(0.0), px(1.5)),
        blur_radius: px(2.5),
        spread_radius: px(0.0),
        inset: true,
    }]
}

/// Floating panel: a lit top edge and a wide, soft shadow.
pub fn panel(dark: bool) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: white(if dark { 0.05 } else { 0.7 }),
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(0.0),
            spread_radius: px(0.0),
            inset: true,
        },
        BoxShadow {
            color: black(if dark { 0.25 } else { 0.09 }),
            offset: point(px(0.0), px(2.0)),
            blur_radius: px(10.0),
            spread_radius: px(0.0),
            inset: false,
        },
    ]
}

/// The same shadows at `t` of their strength, for elements fading in or out.
pub fn faded(mut shadows: Vec<BoxShadow>, t: f32) -> Vec<BoxShadow> {
    let t = t.clamp(0.0, 1.0);
    for shadow in &mut shadows {
        shadow.color.alpha *= t;
    }
    shadows
}

/// Coloured glow around an element (current node, focused field).
pub fn glow(color: Rgba, alpha: f32, radius: f32) -> BoxShadow {
    BoxShadow {
        color: to_hsla(crate::color::with_alpha(color, alpha)),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(radius),
        spread_radius: px(radius * 0.25),
        inset: false,
    }
}
