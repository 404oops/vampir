//! Vampir: lit controls for [GPUI](https://www.gpui.rs).
//!
//! The look: surfaces carry a gradient lit from above, controls sit on that
//! with a highlight along their top edge and a soft shadow beneath, and
//! fields are recessed wells. [`lighting`] holds those recipes, [`Palette`]
//! the twenty-odd colours they use, and [`controls`] the widgets built from
//! both.
//!
//! Nothing here knows about any particular application. A host stores a
//! [`ControlState`], implements [`ControlHost`] for its view, and hands
//! controls a [`Palette`]; every control is generic over that view.
//!
//! ```ignore
//! use vampir::{ControlHost, ControlState, Palette, controls, ButtonVariant};
//!
//! struct Editor { controls: ControlState, palette: Palette }
//!
//! impl ControlHost for Editor {
//!     fn control_state(&self) -> &ControlState { &self.controls }
//!     fn control_state_mut(&mut self) -> &mut ControlState { &mut self.controls }
//! }
//!
//! // ...inside Editor::render:
//! controls::button("save", "Save", ButtonVariant::Primary, true, self.palette, cx,
//!     |editor, _window, cx| editor.save(cx))
//! ```
//!
//! A host with no theme of its own can start from
//! [`Palette::from_hue`], which derives the whole set from one hue in
//! degrees plus a light/dark flag.

pub mod color;
pub mod containers;
pub mod controls;
pub mod data;
pub mod easing;
#[cfg(feature = "app-icon")]
pub mod icon;
pub mod keyboard;
pub mod lighting;
pub mod menu;
pub mod overlay;
pub mod palette;
pub mod scroll;
pub mod shortcut;
pub mod state;
pub mod swatch;
pub mod text_input;

pub use containers::{DialogButton, Tab, collapsible, dialog, split_area, split_handle, tab_bar};
pub use controls::{
    BadgeTone, ButtonVariant, CHIP_HEIGHT, CONTROL_HEIGHT, CONTROL_RADIUS, ChipSelection, Choice,
    ComboDirection, SliderTrack, badge, button, caption, checkbox, chip, chip_group, combo,
    icon_button, progress_bar, radio_group, scrollbar, search_field, segmented, separator, slider,
    spinbox, spinner, switch, text_area, text_field,
};
pub use data::{
    Column, SortDirection, TABLE_ROW_HEIGHT, TreeMove, TreeRow, table_cell, table_header, tree_row,
    tree_step,
};
pub use easing::{ease_in_cubic, ease_out_cubic, modal_opacity};
#[cfg(feature = "app-icon")]
pub use icon::AppIcon;
pub use keyboard::{Key, Orientation, key, move_focus, nudge, nudge_stepped, ring, ring_for, step};
pub use lighting::{glow, lit, lit_at, lit_mix, lit_stops, panel, raised, recessed, rim, shade};
pub use menu::{ContextMenu, MenuAction, MenuItem, context_menu, menu_button};
pub use overlay::{Command, Tooltip, command_list, command_palette, fuzzy_filter, fuzzy_score};
pub use palette::Palette;
pub use scroll::{SCROLL_FADE, ScrollAxis, ScrollDrag, apply_scroll_drag, scroll_fades};
pub use shortcut::{Chord, display, display_keystroke, shortcut_recorder};
pub use state::{
    COMBO_REVEAL, ComboId, ControlHost, ControlState, MOVE, OpenMenu, SCHEME_FADE, SWITCH_SLIDE,
    TabDrag, TrackAxis, continue_drags, end_drags,
};
pub use swatch::{MAX_CHROMA, Oklch, color_pad, hue_slider, hue_wheel, swatch_grid};
pub use text_input::{Highlighter, InputStyle, Span, TextInput};
