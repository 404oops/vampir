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

/// The faces each platform keeps its symbols in — geometric shapes, dingbats,
/// arrows — in the order to try them after the interface face. GPUI-CE's text
/// system falls back to a short list of text faces, none of which has them,
/// and draws a stand-in for a symbol it cannot find. A face a machine does not
/// have is skipped.
pub fn symbol_fonts() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["Apple Symbols", "Menlo"]
    } else if cfg!(target_os = "windows") {
        &["Segoe UI Symbol", "Segoe UI Emoji"]
    } else {
        &["DejaVu Sans", "Noto Sans Symbols 2", "Noto Sans Symbols"]
    }
}

/// [`ui_font`] with [`symbol_fonts`] behind it. Put it on the root with
/// `.font(ui_font_face())`, so a symbol in a label or a menu icon is drawn
/// rather than a stand-in for it.
pub fn ui_font_face() -> gpui::Font {
    gpui::Font {
        fallbacks: Some(gpui::FontFallbacks::from_fonts(
            symbol_fonts().iter().map(|face| face.to_string()).collect(),
        )),
        ..gpui::font(ui_font())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interface_face_falls_back_to_the_symbol_faces_in_order() {
        let face = ui_font_face();
        assert_eq!(face.family.as_ref(), ui_font());
        let fallbacks = face
            .fallbacks
            .expect("symbol faces behind the interface face");
        assert_eq!(fallbacks.fallback_list(), symbol_fonts());
    }
}
