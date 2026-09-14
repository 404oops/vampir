//! The theme a host has when it has none of its own: a hue and saturation, a colour
//! scheme that follows the desktop until someone chooses otherwise, and the
//! palette derived from them — cross-fading rather than cutting when any
//! changes.
//!
//! [`ControlState`](crate::ControlState) carries one [`Theme`];
//! [`ControlState::palette`](crate::ControlState::palette) is the palette to
//! hand every control this frame, and
//! [`ControlState::observe_appearance`](crate::ControlState::observe_appearance)
//! keeps the scheme in step with the desktop. A host with a richer theme of
//! its own ignores all of this and builds its [`Palette`] however it likes.
//!
//! ```ignore
//! // In the view's constructor:
//! self.controls.observe_appearance(window, cx);
//!
//! // In render:
//! let palette = self.controls.palette();
//! ```

use std::cell::Cell;

use gpui::{AnyElement, Subscription, Window, WindowAppearance, prelude::*};

use crate::controls::WidgetContext;

use crate::controls::segmented;
use crate::palette::{DEFAULT_SATURATION, MAX_SATURATION, Palette, normalized_saturation};
use crate::state::{ComboId, ControlHost};
use crate::swatch::{hue_slider, saturation_slider};

/// Which colour scheme a theme is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scheme {
    /// Whatever the desktop is in, and whatever it changes to.
    #[default]
    System,
    Light,
    Dark,
}

impl Scheme {
    /// The three, in the order a picker lists them.
    pub const ALL: [Scheme; 3] = [Scheme::System, Scheme::Light, Scheme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Scheme::System => "System",
            Scheme::Light => "Light",
            Scheme::Dark => "Dark",
        }
    }

    /// Position in [`Scheme::ALL`], for a segmented control or a menu.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|scheme| *scheme == self)
            .unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Option<Scheme> {
        Self::ALL.get(index).copied()
    }
}

/// The hue, saturation and scheme a host's palette is derived from. See the module
/// documentation.
pub struct Theme {
    /// The accent hue in degrees, 0 to 360.
    pub hue: f64,
    /// Colour intensity: 0 is grey, 1 is the default and 2 is vivid.
    /// Values are clamped to this range when deriving the palette.
    pub saturation: f64,
    pub scheme: Scheme,
    /// Whether the desktop is dark right now. Kept up to date by
    /// [`ControlState::observe_appearance`]; what [`Scheme::System`] means.
    system_dark: bool,
    /// The slider that drags the hue, if the host shows one. While it is
    /// held the colours sit under the hand rather than gliding after it.
    hue_track: Cell<Option<ComboId>>,
    saturation_track: Cell<Option<ComboId>>,
    /// Keeps the appearance observer alive for as long as the state is.
    appearance: Option<Subscription>,
    /// A settled theme is reused across pointer moves and unrelated frames.
    palettes: [Cell<Option<CachedPalette>>; 2],
}

#[derive(Clone, Copy)]
struct CachedPalette {
    hue: u64,
    saturation: u64,
    palette: Palette,
}

/// The fallback hue. Applications should choose their own hue and
/// saturation to fit their purpose and visual identity.
pub const DEFAULT_HUE: f64 = 268.0;

impl Default for Theme {
    fn default() -> Self {
        Self {
            hue: DEFAULT_HUE,
            saturation: DEFAULT_SATURATION,
            scheme: Scheme::System,
            system_dark: false,
            hue_track: Cell::new(None),
            saturation_track: Cell::new(None),
            appearance: None,
            palettes: [Cell::new(None), Cell::new(None)],
        }
    }
}

impl Theme {
    /// Whether the palette should be dark: the scheme's own answer, or the
    /// desktop's while the scheme is [`Scheme::System`].
    pub fn is_dark(&self) -> bool {
        match self.scheme {
            Scheme::System => self.system_dark,
            Scheme::Light => false,
            Scheme::Dark => true,
        }
    }

    /// Whether the desktop is dark, whatever the scheme says.
    pub fn system_is_dark(&self) -> bool {
        self.system_dark
    }

    /// Records what the desktop is showing. Returns whether that changed.
    pub fn set_system_dark(&mut self, dark: bool) -> bool {
        let changed = self.system_dark != dark;
        self.system_dark = dark;
        changed
    }

    /// The slider that is dragging the hue, if any. Set by [`hue_picker`].
    pub fn hue_track(&self) -> Option<ComboId> {
        self.hue_track.get()
    }

    /// The slider bound to saturation, if any. Set by [`saturation_picker`].
    pub fn saturation_track(&self) -> Option<ComboId> {
        self.saturation_track.get()
    }

    pub(crate) fn apply_track(&mut self, id: ComboId, at: f32) -> bool {
        if self.hue_track() == Some(id) {
            self.hue = Self::hue_from_track(at);
        } else if self.saturation_track() == Some(id) {
            self.saturation = Self::saturation_from_track(at);
        } else {
            return false;
        }
        true
    }

    /// Keeps the desktop observer alive for as long as the theme is.
    pub(crate) fn keep_appearance(&mut self, subscription: Subscription) {
        self.appearance = Some(subscription);
    }

    /// Reads a hue off a track: the 0..=1 position a drag reports, as
    /// degrees, stopping just short of a full turn so the far end of the
    /// slider does not wrap back to the near one.
    pub fn hue_from_track(at: f32) -> f64 {
        (f64::from(at) * 360.0).clamp(0.0, 359.9)
    }

    /// Reads a 0..=1 track position as saturation: 0 is grey, the middle is
    /// the default intensity and the far end is [`MAX_SATURATION`].
    pub fn saturation_from_track(at: f32) -> f64 {
        normalized_saturation(f64::from(at) * MAX_SATURATION)
    }

    /// The palette at the theme's own hue, saturation and scheme, with no motion. The
    /// animated one is [`ControlState::palette`](crate::ControlState::palette).
    pub fn palette(&self) -> Palette {
        self.palette_at(self.hue, self.saturation, self.is_dark())
    }

    pub(crate) fn palette_at(&self, hue: f64, saturation: f64, dark: bool) -> Palette {
        let saturation = normalized_saturation(saturation);
        let cache = &self.palettes[usize::from(dark)];
        if let Some(cached) = cache.get()
            && cached.hue == hue.to_bits()
            && cached.saturation == saturation.to_bits()
        {
            return cached.palette;
        }
        let palette = Palette::from_hue_and_saturation(hue, saturation, dark);
        cache.set(Some(CachedPalette {
            hue: hue.to_bits(),
            saturation: saturation.to_bits(),
            palette,
        }));
        palette
    }
}

/// Whether the OS is showing windows dark right now. GPUI folds the vibrant
/// variants in with their plain ones for this purpose.
pub fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// A segmented control that picks the theme's [`Scheme`]: System, Light or
/// Dark. Choosing System hands control back to the desktop.
pub fn scheme_picker<V: ControlHost>(id: ComboId, ctx: WidgetContext<'_, '_, '_, V>) -> AnyElement {
    let options: Vec<String> = Scheme::ALL
        .iter()
        .map(|scheme| scheme.label().to_string())
        .collect();
    // Boxed rather than `impl IntoElement`: in the 2024 edition an opaque
    // return would keep `options` borrowed past the end of this function.
    let scheme = ctx.state().theme.scheme.index();
    segmented(
        id,
        &options,
        scheme,
        true,
        ctx,
        |this, index, _window, cx| {
            if let Some(scheme) = Scheme::from_index(index) {
                this.control_state_mut().theme.scheme = scheme;
                cx.notify();
            }
        },
    )
    .into_any_element()
}

/// A hue slider bound to the theme's hue. The drag is handled by the
/// toolkit, so the host's `track_dragged` never hears about it.
pub fn hue_picker<V: ControlHost>(
    id: ComboId,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let theme = &ctx.state().theme;
    theme.hue_track.set(Some(id));
    hue_slider(id, theme.hue, ctx)
}

/// A saturation slider bound to the theme. Add it when the application
/// offers colour customization; a host can otherwise set [`Theme::saturation`]
/// directly. Pointer and keyboard changes are handled by the toolkit.
pub fn saturation_picker<V: ControlHost>(
    id: ComboId,
    ctx: WidgetContext<'_, '_, '_, V>,
) -> impl IntoElement {
    let theme = &ctx.state().theme;
    theme.saturation_track.set(Some(id));
    saturation_slider(id, theme.saturation, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_the_desktop_and_a_choice_does_not() {
        let mut theme = Theme::default();
        assert!(!theme.is_dark());
        assert!(theme.set_system_dark(true));
        assert!(theme.is_dark());
        theme.scheme = Scheme::Light;
        assert!(!theme.is_dark());
        theme.scheme = Scheme::Dark;
        theme.set_system_dark(false);
        assert!(theme.is_dark());
    }

    #[test]
    fn schemes_round_trip_through_their_index() {
        for scheme in Scheme::ALL {
            assert_eq!(Scheme::from_index(scheme.index()), Some(scheme));
        }
        assert_eq!(Scheme::from_index(3), None);
    }

    /// The far end of the slider is just short of a full turn, so it does
    /// not read as the same colour as the near end.
    #[test]
    fn a_track_never_reaches_a_full_turn() {
        assert_eq!(Theme::hue_from_track(0.0), 0.0);
        assert!(Theme::hue_from_track(1.0) < 360.0);
        assert!((Theme::hue_from_track(0.5) - 180.0).abs() < 1e-9);
    }

    #[test]
    fn saturation_tracks_map_the_full_range_and_normalize_invalid_positions() {
        for (position, expected) in [
            (0.0, 0.0),
            (0.5, DEFAULT_SATURATION),
            (1.0, MAX_SATURATION),
            (-1.0, 0.0),
            (2.0, MAX_SATURATION),
            (f32::NEG_INFINITY, 0.0),
            (f32::INFINITY, MAX_SATURATION),
            (f32::NAN, DEFAULT_SATURATION),
        ] {
            assert_eq!(Theme::saturation_from_track(position), expected);
        }
    }

    #[test]
    fn bound_tracks_update_their_theme_value_and_leave_other_tracks_to_the_host() {
        let mut theme = Theme::default();
        theme.hue_track.set(Some("theme-hue"));
        theme.saturation_track.set(Some("theme-saturation"));
        assert!(theme.apply_track("theme-hue", 0.5));
        assert_eq!(theme.hue, 180.0);
        assert_eq!(theme.saturation, DEFAULT_SATURATION);
        for position in [0.0, 0.5, 1.0] {
            assert!(theme.apply_track("theme-saturation", position));
            assert_eq!(theme.saturation, Theme::saturation_from_track(position));
            assert_eq!(theme.hue, 180.0);
        }
        assert!(!theme.apply_track("host-slider", 0.0));
        assert_eq!(theme.saturation, MAX_SATURATION);
        assert_eq!(theme.hue, 180.0);
    }

    #[test]
    fn saturation_animates_programmatic_changes_and_snaps_during_its_own_drag() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            let mut state = crate::ControlState::new();
            state.theme.scheme = scheme;
            state.theme.saturation_track.set(Some("saturation"));
            let initial = state.palette();
            state.theme.saturation = 0.0;
            assert_eq!(state.palette(), initial);
            assert!(state.animating());

            state.begin_track_drag("saturation", crate::state::TrackAxis::Horizontal, None);
            assert_eq!(state.palette(), state.theme.palette());
            state.theme.saturation = MAX_SATURATION;
            assert_eq!(state.palette(), state.theme.palette());
            state.end_drag();
            assert_eq!(state.palette(), state.theme.palette());

            let settled = state.palette();
            state.begin_track_drag("unrelated", crate::state::TrackAxis::Horizontal, None);
            state.theme.saturation = 0.0;
            assert_eq!(state.palette(), settled);
        }
    }

    #[test]
    fn palette_cache_tracks_direct_hue_saturation_and_scheme_changes() {
        let mut theme = Theme::default();
        assert_eq!(theme.palette(), Palette::from_hue(DEFAULT_HUE, false));
        theme.hue = 37.25;
        assert_eq!(theme.palette(), Palette::from_hue(37.25, false));
        theme.scheme = Scheme::Dark;
        assert_eq!(theme.palette(), Palette::from_hue(37.25, true));
        theme.hue = 201.5;
        assert_eq!(theme.palette(), Palette::from_hue(201.5, true));
        theme.scheme = Scheme::System;
        assert_eq!(theme.palette(), Palette::from_hue(201.5, false));
        theme.set_system_dark(true);
        assert_eq!(theme.palette(), Palette::from_hue(201.5, true));
        for saturation in [0.0, 0.5, MAX_SATURATION, DEFAULT_SATURATION] {
            theme.saturation = saturation;
            for scheme in [Scheme::Light, Scheme::Dark] {
                theme.scheme = scheme;
                assert_eq!(
                    theme.palette(),
                    Palette::from_hue_and_saturation(201.5, saturation, theme.is_dark())
                );
            }
        }
        theme.saturation = f64::NAN;
        assert_eq!(theme.palette(), Palette::from_hue(201.5, true));
    }
}
