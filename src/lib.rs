//! Vampir: lit controls for [GPUI](https://www.gpui.rs).
//!
//! The look: surfaces carry a gradient lit from above, controls sit on that
//! with a highlight along their top edge and a soft shadow beneath, and
//! fields are recessed wells. [`lighting`] holds those recipes, [`Palette`]
//! the twenty-odd colours they use, and [`controls`] the widgets built from
//! both.
//!
//! Nothing here knows about any particular application. A host stores a
//! [`ControlState`], implements [`ControlHost`] for its view, puts the
//! toolkit's handlers on its root with [`root`], and hands controls a
//! [`Palette`]; every control is generic over that view.
//!
//! ```ignore
//! use vampir::{ControlHost, ControlState, controls, ButtonVariant};
//!
//! struct Editor { controls: ControlState }
//!
//! impl ControlHost for Editor {
//!     fn control_state(&self) -> &ControlState { &self.controls }
//!     fn control_state_mut(&mut self) -> &mut ControlState { &mut self.controls }
//! }
//!
//! // ...inside Editor::render:
//! let palette = self.controls.palette();
//! vampir::root(div().id("root"), self, cx)
//!     .child(controls::button("save", "Save", ButtonVariant::Primary, true, palette, cx,
//!         |editor, _window, cx| editor.save(cx)))
//! ```
//!
//! [`ControlState::palette`] is the palette of the [`theme`] every host
//! has by default — an app-chosen hue and saturation, and the desktop's
//! colour scheme — crossing over rather than cutting when they change.
//! A host with a theme of its own builds a [`Palette`] from it instead, with
//! [`Palette::from_hue_and_saturation`] or field by field.

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
pub mod theme;
pub mod typography;

pub use containers::{
    DialogButton, Tab, arriving, card, collapsible, column, dialog, labelled, reorder, row,
    scroll_area, split_area, split_handle, tab_bar,
};
pub use controls::{
    BadgeTone, ButtonVariant, CHIP_HEIGHT, CONTROL_HEIGHT, CONTROL_RADIUS, ChipSelection, Choice,
    SliderTrack, WidgetContext, badge, button, caption, checkbox, chip, chip_group, combo,
    fading_text, glyph, icon_button, progress_bar, radio_group, scrollbar, search_field, segmented,
    separator, slider, spinbox, spinner, switch, text_area, text_field,
};
pub use data::{
    Column, SortDirection, TABLE_ROW_HEIGHT, TreeMove, TreeRow, flatten_tree, table_cell,
    table_header, table_row, tree_row, tree_step,
};
pub use easing::{ease_in_cubic, ease_out_cubic, modal_opacity};
#[cfg(feature = "app-icon")]
pub use icon::AppIcon;
pub use keyboard::{
    Dismiss, FocusNext, FocusPrevious, Key, Orientation, bind_keys, edit_menu, handle_keys, key,
    move_focus, nudge, nudge_stepped, ring, ring_for, root, standard_bindings, step,
};
pub use lighting::{
    glow, ground, ground_at, lit, lit_at, lit_mix, lit_stops, panel, raised, recessed, rim, shade,
};
pub use menu::{ContextMenu, MenuAction, MenuItem, context_menu, menu_button, menu_target};
pub use overlay::{
    Command, Hint, SearchResult, Tooltip, command_list, command_palette, fuzzy_filter, fuzzy_score,
    ranked_command_palette, ranked_search_list, search_list,
};
pub use palette::{DEFAULT_SATURATION, MAX_SATURATION, Palette};
pub use scroll::{SCROLL_FADE, ScrollAxis, ScrollDrag, apply_scroll_drag, scroll_fades};
pub use shortcut::{Chord, display, display_keystroke, shortcut_recorder};
pub use state::{
    COMBO_REVEAL, ComboId, ControlHost, ControlState, MOVE, OpenCombo, OpenDialog, OpenMenu,
    OpenPalette, SCHEME_FADE, SWITCH_SLIDE, TAB_DRAG_THRESHOLD, TabDrag, Tag, TrackAxis, TrackDrag,
    continue_drags, end_drags, handle_mouse, mouse_moved,
};
pub use swatch::{
    MAX_CHROMA, Oklch, color_pad, hue_slider, hue_wheel, saturation_slider, swatch_grid,
};
pub use text_input::{Highlighter, InputStyle, Span, TextInput};
pub use theme::{
    DEFAULT_HUE, Scheme, Theme, hue_picker, saturation_picker, scheme_picker, system_dark,
};
pub use typography::{SMALL_TEXT_SIZE, TEXT_SIZE, TITLE_TEXT_SIZE, ui_font};
