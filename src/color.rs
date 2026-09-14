//! Colour maths for building a [`crate::Palette`].
//!
//! Palettes are written in OKLCH because it keeps lightness and saturation
//! perceptually even as the hue turns. That is what lets a whole theme
//! follow one hue slider without some hues going muddy and others glaring.

use gpui::{Hsla, Rgba};

// GPUI's colours are the `palette` crate's: `Rgba` is an `Alpha<Rgb, f32>`
// with a `color` and an `alpha`, not four flat fields. These are the handful
// of ways the toolkit touches one, so nothing else has to know that.

/// A colour from its four channels, 0..=1.
pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba::new(r, g, b, a)
}

/// The same colour at a different opacity.
pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { alpha, ..color }
}

/// The channels, `[r, g, b, a]`.
pub fn channels(color: Rgba) -> [f32; 4] {
    [
        color.color.red,
        color.color.green,
        color.color.blue,
        color.alpha,
    ]
}

/// For the GPUI APIs that take an `Hsla` — shadows, paths, text runs.
pub fn to_hsla(color: Rgba) -> Hsla {
    gpui::rgb_to_hsla(color)
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

pub fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Gamma-encodes a linear channel, clamping to the displayable range first.
pub fn linear_to_srgb(v: f64) -> f64 {
    let c = clamp01(v);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Perceived brightness, 0..=1. Use it to decide whether a label on a given
/// fill should be dark or light.
pub fn relative_luminance(color: Rgba) -> f64 {
    let [r, g, b, _] = channels(color).map(f64::from);
    let (r, g, b) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// OKLCH to sRGB, through the standard OKLab matrices: `l` is lightness
/// 0..=1, `c` chroma, `h` hue in degrees. Lightness is clamped, chroma
/// floored at zero, and the linear channels clamped before gamma encoding,
/// so an out-of-gamut request desaturates instead of wrapping to some other
/// colour.
pub fn oklch_to_color(l_in: f64, c_in: f64, h_degrees: f64) -> Rgba {
    OklchHue::new(h_degrees).color(l_in, c_in)
}

/// A pad varies chroma and lightness thousands of times at one hue.
#[derive(Clone, Copy)]
pub(crate) struct OklchHue {
    cos: f64,
    sin: f64,
}

impl OklchHue {
    pub(crate) fn new(degrees: f64) -> Self {
        let radians = (degrees % 360.0) * std::f64::consts::PI / 180.0;
        Self {
            cos: radians.cos(),
            sin: radians.sin(),
        }
    }

    pub(crate) fn color(self, l_in: f64, c_in: f64) -> Rgba {
        let l = clamp01(l_in);
        let c = c_in.max(0.0);

        let a_ = c * self.cos;
        let b_ = c * self.sin;

        let l_ = l + 0.3963377774 * a_ + 0.2158037573 * b_;
        let m_ = l - 0.1055613458 * a_ - 0.0638541728 * b_;
        let s_ = l - 0.0894841775 * a_ - 1.2914855480 * b_;

        let l3 = l_ * l_ * l_;
        let m3 = m_ * m_ * m_;
        let s3 = s_ * s_ * s_;

        let r_lin = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
        let g_lin = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
        let b_lin = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;

        rgba(
            linear_to_srgb(r_lin) as f32,
            linear_to_srgb(g_lin) as f32,
            linear_to_srgb(b_lin) as f32,
            1.0,
        )
    }
}

/// `0xAARRGGBB` literal, for the few colours that are plain black or white
/// overlays rather than part of a hue-driven palette.
pub const fn argb(hex: u32) -> Rgba {
    rgba(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
        ((hex >> 24) & 0xff) as f32 / 255.0,
    )
}

/// Blends two colours in linear light, which is closer to the OKLCH
/// pipeline's working space than a raw sRGB mix and keeps mid-blends from
/// dipping dark.
pub fn mix(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let channel = |a: f32, b: f32| {
        let a = srgb_to_linear(a as f64);
        let b = srgb_to_linear(b as f64);
        linear_to_srgb(a + (b - a) * t as f64) as f32
    };
    let [fr, fg, fb, fa] = channels(from);
    let [tr, tg, tb, ta] = channels(to);
    rgba(
        channel(fr, tr),
        channel(fg, tg),
        channel(fb, tb),
        fa + (ta - fa) * t,
    )
}

/// Straight-line blend in sRGB. Cheaper than [`mix`] and what the control
/// animations use, where the two ends are close enough that the working
/// space does not show.
pub fn lerp(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let channel = |a: f32, b: f32| a + (b - a) * t;
    let [fr, fg, fb, fa] = channels(from);
    let [tr, tg, tb, ta] = channels(to);
    rgba(
        channel(fr, tr),
        channel(fg, tg),
        channel(fb, tb),
        channel(fa, ta),
    )
}

pub const WHITE: Rgba = rgba(1.0, 1.0, 1.0, 1.0);

pub const TRANSPARENT: Rgba = rgba(0.0, 0.0, 0.0, 0.0);
