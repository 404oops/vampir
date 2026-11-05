//! Type: the face the toolkit's text is set in, and the three sizes it uses.

/// Text size of a control's label, and of body text between controls.
pub const TEXT_SIZE: f32 = 12.5;
/// Captions, shortcuts, the quieter line under a label.
pub const SMALL_TEXT_SIZE: f32 = 11.5;
/// A dialog's title, a command palette's query.
pub const TITLE_TEXT_SIZE: f32 = 14.0;

/// The system's interface face, under the name each platform knows it by.
///
/// An unknown name falls back to *something*, but not to the same something
/// everywhere, and not to the face the rest of the desktop is set in. Put it
/// on the root with `.font_family(ui_font())`.
pub fn ui_font() -> &'static str {
    if cfg!(target_os = "macos") {
        ".SystemUIFont"
    } else if cfg!(target_os = "windows") {
        "Segoe UI"
    } else {
        "sans-serif"
    }
}
