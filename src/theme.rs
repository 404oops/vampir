//! The theme a host has when it has none of its own: one hue, a colour
//! scheme that follows the desktop until someone chooses otherwise, and the
//! palette derived from both — cross-fading rather than cutting when either
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

use gpui::{AnyElement, Context, Subscription, Window, WindowAppearance, prelude::*};

use crate::controls::segmented;
use crate::palette::Palette;
use crate::state::{ComboId, ControlHost};
use crate::swatch::hue_slider;

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

/// The hue and scheme a host's palette is derived from. See the module
/// documentation.
pub struct Theme {
    /// The accent hue in degrees, 0 to 360.
    pub hue: f64,
    pub scheme: Scheme,
    /// Whether the desktop is dark right now. Kept up to date by
    /// [`ControlState::observe_appearance`]; what [`Scheme::System`] means.
    system_dark: bool,
    /// The slider that drags the hue, if the host shows one. While it is
    /// held the colours sit under the hand rather than gliding after it.
    hue_track: Cell<Option<ComboId>>,
    /// Keeps the appearance observer alive for as long as the state is.
    appearance: Option<Subscription>,
}

/// The hue the toolkit starts on: a violet that reads as neither warm nor
/// cold, so nothing in a first screenshot looks like a brand decision.
pub const DEFAULT_HUE: f64 = 268.0;

impl Default for Theme {
    fn default() -> Self {
        Self {
            hue: DEFAULT_HUE,
            scheme: Scheme::System,
            system_dark: false,
            hue_track: Cell::new(None),
            appearance: None,
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

    /// The palette at the theme's own hue and scheme, with no motion. The
    /// animated one is [`ControlState::palette`](crate::ControlState::palette).
    pub fn palette(&self) -> Palette {
        Palette::from_hue(self.hue, self.is_dark())
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
pub fn scheme_picker<V: ControlHost>(
    id: ComboId,
    palette: Palette,
    view: &V,
    cx: &mut Context<V>,
) -> AnyElement {
    let options: Vec<String> = Scheme::ALL
        .iter()
        .map(|scheme| scheme.label().to_string())
        .collect();
    // Boxed rather than `impl IntoElement`: in the 2024 edition an opaque
    // return would keep `options` borrowed past the end of this function.
    segmented(
        id,
        &options,
        view.control_state().theme.scheme.index(),
        true,
        palette,
        view,
        cx,
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
    palette: Palette,
    view: &V,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let theme = &view.control_state().theme;
    theme.hue_track.set(Some(id));
    hue_slider(id, theme.hue, palette, view, cx)
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
}
