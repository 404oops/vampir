//! Text input entity: single-line fields and wrapped multi-line areas,
//! with selection, IME composition, undo and a system clipboard.
//!
//! Rich text is a [`Highlighter`] on the input: a closure from the current
//! content to a list of [`Span`]s, re-run whenever the text changes. That
//! covers syntax highlighting, search matches, spell squiggles and mention
//! chips without the input having to know what any of them are.
//!
//! Chrome (the border, background and padding) belongs to whoever places the
//! input: see [`crate::controls::text_field`] and
//! [`crate::controls::text_area`].
//!
//! Adapted from gpui's `input.rs` example, extended with multi-line
//! wrapping, styled colors, highlight spans, and a change callback.

use std::{ops::Range, rc::Rc};

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Rgba, SharedString,
    Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*,
    px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::palette::Palette;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        SelectToHome,
        SelectToEnd,
        DocumentStart,
        DocumentEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteToLineStart,
        Undo,
        Redo,
        Enter,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
    ]
);

/// A styled stretch of the content, produced by a [`Highlighter`].
///
/// Ranges are byte offsets into the content and may not overlap; where two
/// spans would, the first one wins. A range that falls outside the content
/// is ignored rather than panicking, because a highlighter usually runs on
/// text that has since been edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    /// Overrides the input's text colour for this stretch.
    pub color: Option<Rgba>,
    /// Painted behind the text. The selection is drawn over it.
    pub background: Option<Rgba>,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<Rgba>,
    /// A wavy underline in `underline`'s colour, for errors.
    pub wavy: bool,
    pub strikethrough: bool,
    /// Set in the input's accent colour, whatever that is when it paints,
    /// so a highlighter written once follows the theme.
    pub accent: bool,
}

impl Span {
    pub fn new(range: Range<usize>) -> Self {
        Self {
            range,
            color: None,
            background: None,
            bold: false,
            italic: false,
            underline: None,
            wavy: false,
            strikethrough: false,
            accent: false,
        }
    }

    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
        self
    }

    /// The input's accent colour — [`InputStyle::accent_color`] — read when
    /// the text paints rather than when the span is made. A highlighter is
    /// usually set once and the theme changes under it; a colour captured in
    /// the closure would stay behind.
    pub fn accent(mut self) -> Self {
        self.accent = true;
        self
    }

    pub fn background(mut self, color: Rgba) -> Self {
        self.background = Some(color);
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn underline(mut self, color: Rgba) -> Self {
        self.underline = Some(color);
        self
    }

    /// A wavy underline, the usual mark for a spelling or syntax error.
    pub fn squiggle(mut self, color: Rgba) -> Self {
        self.underline = Some(color);
        self.wavy = true;
        self
    }

    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }
}

/// Notified with the input's content. Used for both the change and the
/// submit hooks.
pub type ContentCallback = Box<dyn Fn(&str, &mut App) + 'static>;

/// Turns content into styled spans. Called on every layout, so keep it
/// cheap or cache inside the closure.
pub type Highlighter = Box<dyn Fn(&str) -> Vec<Span> + 'static>;

/// Colors for the input text; the parent owns the container chrome.
///
/// [`InputStyle::from_palette`] derives one from a [`Palette`], and an input
/// styled that way is re-styled by the field that holds it whenever the
/// palette changes, so a theme crossing over carries the text with it. A
/// style written out by hand is left alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputStyle {
    pub text_color: Rgba,
    pub placeholder_color: Rgba,
    pub selection_color: Rgba,
    pub cursor_color: Rgba,
    /// What a [`Span::accent`] paints in.
    pub accent_color: Rgba,
    pub font_size: f32,
    /// Whether the colours came from a palette and should follow it. Set by
    /// [`InputStyle::from_palette`]; a style built by hand keeps its colours
    /// whatever palette the field around it is handed.
    pub follows_palette: bool,
}

impl InputStyle {
    /// The palette's text, placeholder, selection and caret colours, at
    /// `font_size`. The input then follows the palette; see
    /// [`TextInput::restyle`].
    pub fn from_palette(palette: Palette, font_size: f32) -> Self {
        Self {
            text_color: palette.text_primary,
            placeholder_color: palette.text_secondary,
            selection_color: palette.accent,
            cursor_color: palette.accent,
            accent_color: palette.accent,
            font_size,
            follows_palette: true,
        }
    }
}

pub struct TextInput {
    pub focus_handle: FocusHandle,
    pub content: SharedString,
    pub placeholder: SharedString,
    pub multi_line: bool,
    pub style: InputStyle,
    pub disabled: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_lines: Rc<[gpui::WrappedLine]>,
    shaped: Option<ShapedText>,
    last_line_height: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    // Horizontal scroll keeping the caret visible in single-line fields.
    scroll_offset: Pixels,
    undo_stack: Vec<(SharedString, Range<usize>)>,
    redo_stack: Vec<(SharedString, Range<usize>)>,
    pub on_change: Option<ContentCallback>,
    /// Enter in a single-line field commits through this (e.g. tab rename).
    pub on_submit: Option<ContentCallback>,
    /// Rich text: styles stretches of the content as it is laid out.
    pub highlighter: Option<Highlighter>,
}

const UNDO_LIMIT: usize = 200;

impl TextInput {
    pub fn new(
        cx: &mut Context<Self>,
        placeholder: &str,
        multi_line: bool,
        style: InputStyle,
    ) -> Self {
        Self {
            // A tab stop, so Tab reaches the field. `track_focus` alone
            // only makes an element focusable — it does not put it in the
            // tab order, which is why a text field that took the mouse
            // happily could not be reached from the keyboard at all.
            focus_handle: cx.focus_handle().tab_stop(true),
            content: "".into(),
            placeholder: SharedString::from(placeholder),
            multi_line,
            style,
            disabled: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_lines: Rc::from([]),
            shaped: None,
            last_line_height: px(0.0),
            last_bounds: None,
            is_selecting: false,
            scroll_offset: px(0.0),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            on_change: None,
            on_submit: None,
            highlighter: None,
        }
    }

    /// Styles the content as it is typed. See [`Span`].
    pub fn set_highlighter(
        &mut self,
        highlighter: impl Fn(&str) -> Vec<Span> + 'static,
        cx: &mut Context<Self>,
    ) {
        self.highlighter = Some(Box::new(highlighter));
        cx.notify();
    }

    pub fn clear_highlighter(&mut self, cx: &mut Context<Self>) {
        self.highlighter = None;
        cx.notify();
    }

    /// Brings a palette-derived style up to date with `palette`, keeping
    /// the font size. The fields that hold an input call this as they
    /// render, so a host never re-styles its inputs by hand; a style that
    /// did not come from a palette is left alone. Returns whether anything
    /// changed.
    pub fn restyle(&mut self, palette: Palette) -> bool {
        if !self.style.follows_palette {
            return false;
        }
        let fresh = InputStyle::from_palette(palette, self.style.font_size);
        if self.style == fresh {
            return false;
        }
        self.style = fresh;
        true
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = SharedString::from(text);
        let len = self.content.len();
        self.selected_range = len..len;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_offset = px(0.0);
        // Programmatic loads are a new document; edits shouldn't undo into
        // the previous one.
        self.undo_stack.clear();
        self.redo_stack.clear();
        cx.notify();
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    /// Click on the field's chrome (padding, empty area below the text):
    /// focus the input, and when the click missed the text itself, put the
    /// caret at the end, so the field focuses from anywhere in the box.
    pub fn handle_chrome_click(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus_handle, cx);
        // The chrome has no focus handle of its own, so prevent a focusable
        // ancestor from taking this press after we hand it to the input.
        window.prevent_default();
        let inside_text = self
            .last_bounds
            .is_some_and(|bounds| bounds.contains(&position));
        if !inside_text {
            let end = self.content.len();
            self.selected_range = end..end;
            self.selection_reversed = false;
        }
        cx.notify();
    }

    fn emit_change(&mut self, cx: &mut Context<Self>) {
        if let Some(on_change) = self.on_change.take() {
            on_change(&self.content, cx);
            self.on_change = Some(on_change);
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    // A single line has nowhere to move up or down to, so the key is passed
    // on rather than swallowed: whatever holds the field — a spin box, a
    // command palette — is what the arrows mean there. GPUI stops at the
    // first handler that does not say otherwise, so "did nothing" has to be
    // said out loud.
    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_line {
            cx.propagate();
            return;
        }
        self.move_vertically(-1, false, window, cx);
    }

    fn down(&mut self, _: &Down, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_line {
            cx.propagate();
            return;
        }
        self.move_vertically(1, false, window, cx);
    }

    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_line {
            return;
        }
        self.move_vertically(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_line {
            return;
        }
        self.move_vertically(1, true, window, cx);
    }

    fn move_vertically(
        &mut self,
        direction: i32,
        select: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = self.last_line_height;
        if line_height <= px(0.0) {
            return;
        }
        let Some(current) = self.position_for_offset(self.cursor_offset()) else {
            return;
        };
        let target_y = current.y + line_height * direction as f32 + line_height / 2.0;
        let target = point(current.x, target_y);
        let index = self.offset_for_position_local(target);
        if select {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// Start of the hard line ('\n'-bounded) containing `offset`.
    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset.min(self.content.len())]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0)
    }

    /// End of the hard line containing `offset`.
    fn line_end(&self, offset: usize) -> usize {
        let offset = offset.min(self.content.len());
        self.content[offset..]
            .find('\n')
            .map(|idx| offset + idx)
            .unwrap_or(self.content.len())
    }

    /// Start of the word before `offset` (alt-left semantics).
    fn previous_word_boundary(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .take_while(|(idx, _)| *idx < offset)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    /// End of the word after `offset` (alt-right semantics).
    fn next_word_boundary(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .map(|(idx, word)| idx + word.len())
            .find(|end| *end > offset)
            .unwrap_or(self.content.len())
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(self.cursor_offset()), cx);
    }

    fn select_to_home(&mut self, _: &SelectToHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(self.cursor_offset()), cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(self.cursor_offset()), cx);
    }

    fn document_start(&mut self, _: &DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word_boundary(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word_boundary(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word_boundary(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word_boundary(self.cursor_offset()), cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            let boundary = self.previous_word_boundary(self.cursor_offset());
            if boundary == self.cursor_offset() {
                return;
            }
            self.select_to(boundary, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            let start = self.line_start(self.cursor_offset());
            if start == self.cursor_offset() {
                return;
            }
            self.select_to(start, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn push_undo(&mut self) {
        if self
            .undo_stack
            .last()
            .is_some_and(|(content, _)| *content == self.content)
        {
            return;
        }
        if self.undo_stack.len() >= UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.undo_stack
            .push((self.content.clone(), self.selected_range.clone()));
        self.redo_stack.clear();
    }

    fn restore(&mut self, content: SharedString, selection: Range<usize>, cx: &mut Context<Self>) {
        self.content = content;
        let len = self.content.len();
        let start = selection.start.min(len);
        let end = selection.end.min(len);
        self.selected_range = start..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.emit_change(cx);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let Some((content, selection)) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack
            .push((self.content.clone(), self.selected_range.clone()));
        self.restore(content, selection, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let Some((content, selection)) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack
            .push((self.content.clone(), self.selected_range.clone()));
        self.restore(content, selection, cx);
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.multi_line {
            self.replace_text_in_range(None, "\n", window, cx);
        } else if let Some(on_submit) = self.on_submit.take() {
            on_submit(&self.content, cx);
            self.on_submit = Some(on_submit);
        } else {
            // Nothing to submit to: the Enter belongs to whatever holds the
            // field. See `up`.
            cx.propagate();
        }
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        self.is_selecting = true;
        let index = self.index_for_mouse_position(event.position);
        match event.click_count {
            2 => {
                let range = word_range_at(&self.content, index);
                self.move_to(range.start, cx);
                self.select_to(range.end, cx);
            }
            n if n >= 3 => {
                // Triple-click selects the line.
                self.move_to(self.line_start(index), cx);
                self.select_to(self.line_end(index), cx);
            }
            _ => {
                if event.modifiers.shift {
                    self.select_to(index, cx);
                } else {
                    self.move_to(index, cx)
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A release outside the window may never reach on_mouse_up.
        if self.disabled || event.pressed_button != Some(MouseButton::Left) {
            self.is_selecting = false;
        }
        if self.is_selecting {
            let index = self.index_for_mouse_position(event.position);
            self.select_to(index, cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = if self.multi_line {
                text
            } else {
                text.replace('\n', " ")
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    /// Cursor position in text-local coordinates (top-left of glyph line).
    fn position_for_offset(&self, offset: usize) -> Option<Point<Pixels>> {
        position_for_offset(&self.last_lines, self.last_line_height, offset)
    }

    /// Offset for a point in text-local coordinates.
    fn offset_for_position_local(&self, position: Point<Pixels>) -> usize {
        offset_for_position(
            &self.content,
            &self.last_lines,
            self.last_line_height,
            position,
        )
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else {
            return 0;
        };
        let local = point(
            position.x - bounds.left() + self.scroll_offset,
            position.y - bounds.top(),
        );
        self.offset_for_position_local(local)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        utf8_offset_for_utf16(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    text.split_word_bound_indices()
        .find_map(|(start, word)| {
            let end = start + word.len();
            (offset < end || end == text.len()).then_some(start..end)
        })
        .unwrap_or(0..0)
}

fn position_for_offset(
    lines: &[gpui::WrappedLine],
    line_height: Pixels,
    offset: usize,
) -> Option<Point<Pixels>> {
    let mut y_offset = px(0.0);
    let mut start = 0usize;
    for line in lines {
        let end = start + line.text.len();
        if offset <= end {
            let local = line.position_for_index(offset - start, line_height)?;
            return Some(point(local.x, local.y + y_offset));
        }
        y_offset += line.size(line_height).height;
        start = end + 1; // skip the '\n'
    }
    None
}

fn offset_for_position(
    content: &str,
    lines: &[gpui::WrappedLine],
    line_height: Pixels,
    position: Point<Pixels>,
) -> usize {
    if content.is_empty() || position.y < px(0.0) {
        return 0;
    }
    let mut y_offset = px(0.0);
    let mut start = 0usize;
    for line in lines {
        let height = line.size(line_height).height;
        if position.y < y_offset + height {
            let local = point(position.x, position.y - y_offset);
            let index = line
                .closest_index_for_position(local, line_height)
                .unwrap_or_else(|idx| idx);
            // Layout can still describe a placeholder or the preceding edit.
            let mut offset = (start + index.min(line.text.len())).min(content.len());
            while !content.is_char_boundary(offset) {
                offset -= 1;
            }
            return offset;
        }
        y_offset += height;
        start += line.text.len() + 1;
    }
    content.len()
}

fn horizontal_scroll_offset(
    previous: Pixels,
    cursor_x: Pixels,
    text_width: Pixels,
    viewport_width: Pixels,
) -> Pixels {
    let visible_width = (viewport_width - px(2.0)).max(px(0.0));
    let max_scroll = (text_width - visible_width).max(px(0.0));
    previous
        .max(cursor_x - visible_width)
        .min(cursor_x)
        .clamp(px(0.0), max_scroll)
}

fn first_rect_for_range(
    bounds: Bounds<Pixels>,
    start: Point<Pixels>,
    end: Point<Pixels>,
    line_height: Pixels,
    scroll_offset: Pixels,
) -> Bounds<Pixels> {
    // The platform wants the first visual row for its IME candidate window.
    // A range ending on a later row may end to the left of its start.
    let right = if start.y == end.y {
        end.x
    } else {
        bounds.size.width
    };
    Bounds::new(
        point(
            bounds.left() + start.x - scroll_offset,
            bounds.top() + start.y,
        ),
        size((right - start.x).max(px(0.0)), line_height),
    )
}

/// UTF-8 byte offset in `text` for a UTF-16 code-unit offset, clamped to the
/// end of the string.
fn utf8_offset_for_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (utf8_offset, ch) in text.char_indices() {
        if utf16_count >= utf16_offset {
            return utf8_offset;
        }
        utf16_count += ch.len_utf16();
    }
    text.len()
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if self.disabled && !ignore_disabled_input {
            return None;
        }
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        // Committing an IME composition shouldn't undo to the half-composed
        // state; the pre-composition snapshot was taken when marking began.
        if self.marked_range.is_none() {
            self.push_undo();
        }
        self.content = replacing_text(&self.content, range.clone(), new_text).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.emit_change(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        // Snapshot once per composition, before the first marked edit.
        if self.marked_range.is_none() {
            self.push_undo();
        }
        self.content = replacing_text(&self.content, range.clone(), new_text).into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        // The IME selection is relative to the newly inserted text, so it
        // must be resolved against `new_text` (not the whole content) and
        // anchored at the replacement start on both ends.
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| {
                let start = range.start + utf8_offset_for_utf16(new_text, range_utf16.start);
                let end = range.start + utf8_offset_for_utf16(new_text, range_utf16.end);
                start..end
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;

        self.emit_change(cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = self.position_for_offset(range.start)?;
        let end = self.position_for_offset(range.end)?;
        Some(first_rect_for_range(
            bounds,
            start,
            end,
            self.last_line_height,
            self.scroll_offset,
        ))
    }

    fn character_index_for_point(
        &mut self,
        point_: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_bounds?;
        let utf8_index = self.index_for_mouse_position(point_);
        Some(self.offset_to_utf16(utf8_index))
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        !self.disabled
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

// One layout per input, shared with paint and hit testing. GPUI caches glyph
// layouts, but shape_text still copies every hard line and allocates its
// decorations on every call, including repeated layout measurements.
struct ShapedText {
    key: ShapeKey,
    lines: Rc<[gpui::WrappedLine]>,
}

#[derive(PartialEq)]
struct ShapeKey {
    text: SharedString,
    is_placeholder: bool,
    font: gpui::Font,
    style: InputStyle,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    spans: Vec<Span>,
    wrap: Option<Pixels>,
    scale_factor: f32,
}

impl ShapeKey {
    fn selection(range: &Range<usize>, is_placeholder: bool) -> Range<usize> {
        // A caret moves without changing the text's decorations.
        if range.is_empty() || is_placeholder {
            0..0
        } else {
            range.clone()
        }
    }
}

fn replacing_text(text: &str, range: Range<usize>, replacement: &str) -> String {
    let mut result = String::with_capacity(text.len() - range.len() + replacement.len());
    result.push_str(&text[..range.start]);
    result.push_str(replacement);
    result.push_str(&text[range.end..]);
    result
}

struct PrepaintState {
    lines: Rc<[gpui::WrappedLine]>,
    line_height: Pixels,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl TextElement {
    fn shape(
        &self,
        wrap_width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Rc<[gpui::WrappedLine]>, Pixels) {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let marked_range = input.marked_range.clone();
        let style = window.text_style();
        let input_style = input.style;
        let font_size = px(input_style.font_size);
        let multi_line = input.multi_line;

        let display_text = if content.is_empty() {
            input.placeholder.clone()
        } else {
            content
        };
        let is_placeholder = input.content.is_empty();
        // Highlight spans, sanitised: dropped if they fall outside the text
        // or start on a character boundary that no longer exists, and
        // trimmed so none overlaps the one before it.
        let spans = if is_placeholder {
            Vec::new()
        } else {
            input
                .highlighter
                .as_ref()
                .map(|highlight| sanitise_spans(highlight(&display_text), &display_text))
                .unwrap_or_default()
        };

        let key = ShapeKey {
            text: display_text,
            is_placeholder,
            font: style.font(),
            style: input_style,
            selection: ShapeKey::selection(&selected_range, is_placeholder),
            marked: if is_placeholder { None } else { marked_range },
            spans,
            wrap: if multi_line { wrap_width } else { None },
            scale_factor: window.scale_factor(),
        };
        // Highlighters are public callbacks that may read external state,
        // so run them on every layout even when the content is unchanged.
        if let Some(shaped) = &input.shaped
            && shaped.key == key
        {
            return (shaped.lines.clone(), window.line_height());
        }

        let runs = text_runs(&key);
        let lines: Rc<[gpui::WrappedLine]> = window
            .text_system()
            .shape_text(key.text.clone(), font_size, &runs, key.wrap, None)
            .map(|lines| Rc::from(lines.into_vec()))
            .unwrap_or_else(|_| Rc::from([]));
        self.input.update(cx, |input, _| {
            input.shaped = Some(ShapedText {
                key,
                lines: lines.clone(),
            });
        });
        (lines, window.line_height())
    }
}

fn text_runs(key: &ShapeKey) -> Vec<TextRun> {
    let ShapeKey {
        text: display_text,
        is_placeholder,
        font,
        style: input_style,
        selection: selected_range,
        marked: marked_range,
        spans,
        ..
    } = key;
    let base_run = TextRun {
        len: display_text.len(),
        font: font.clone(),
        color: crate::color::to_hsla(if *is_placeholder {
            input_style.placeholder_color
        } else {
            input_style.text_color
        }),
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
    };

    // Split into runs so the selection gets a background color, any
    // marked (IME composition) range gets an underline, and each
    // highlight span gets its own styling.
    if *is_placeholder || (spans.is_empty() && selected_range.is_empty() && marked_range.is_none())
    {
        return vec![base_run];
    }

    let mut runs: Vec<TextRun> = Vec::new();
    let mut boundaries = Vec::with_capacity(6 + spans.len() * 2);
    boundaries.extend([0, display_text.len()]);
    boundaries.push(selected_range.start.min(display_text.len()));
    boundaries.push(selected_range.end.min(display_text.len()));
    if let Some(marked) = marked_range {
        boundaries.push(marked.start.min(display_text.len()));
        boundaries.push(marked.end.min(display_text.len()));
    }
    for span in spans {
        boundaries.push(span.range.start);
        boundaries.push(span.range.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    runs.reserve(boundaries.len().saturating_sub(1));
    let mut spans = spans.iter().peekable();
    for pair in boundaries.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if end <= start {
            continue;
        }
        let mut run = TextRun {
            len: end - start,
            ..base_run.clone()
        };
        // Sanitised spans are ordered and disjoint. Advance once
        // through them instead of searching from the beginning for
        // every run (quadratic for syntax-highlighted documents).
        while spans.peek().is_some_and(|span| span.range.end <= start) {
            spans.next();
        }
        if let Some(span) = spans.peek()
            && span.range.start <= start
            && end <= span.range.end
        {
            if span.accent {
                run.color = crate::color::to_hsla(input_style.accent_color);
            } else if let Some(color) = span.color {
                run.color = crate::color::to_hsla(color);
            }
            if let Some(background) = span.background {
                run.background_color = Some(crate::color::to_hsla(background));
            }
            if span.bold {
                run.font.weight = gpui::FontWeight::BOLD;
            }
            if span.italic {
                run.font.style = gpui::FontStyle::Italic;
            }
            if let Some(color) = span.underline {
                run.underline = Some(UnderlineStyle {
                    color: Some(crate::color::to_hsla(color)),
                    thickness: px(1.0),
                    wavy: span.wavy,
                });
            }
            if span.strikethrough {
                run.strikethrough = Some(gpui::StrikethroughStyle {
                    color: Some(run.color),
                    thickness: px(1.0),
                });
            }
        }
        // The selection wash goes over any span background: while
        // text is selected, that is the state worth seeing.
        if start >= selected_range.start && end <= selected_range.end {
            let mut sel: gpui::Hsla = crate::color::to_hsla(input_style.selection_color);
            sel.alpha = 0.45;
            run.background_color = Some(sel);
        }
        if let Some(marked) = marked_range
            && start >= marked.start
            && end <= marked.end
        {
            run.underline = Some(UnderlineStyle {
                color: Some(run.color),
                thickness: px(1.0),
                wavy: false,
            });
        }
        runs.push(run);
    }
    runs
}

/// Drops spans that no longer fit the text, or that start or end mid
/// character, and trims each to start after the one before it. A
/// highlighter is usually run against text that has since been edited, so
/// stale ranges are expected rather than a bug to panic on.
fn sanitise_spans(mut spans: Vec<Span>, text: &str) -> Vec<Span> {
    spans.retain(|span| {
        span.range.start < span.range.end
            && span.range.end <= text.len()
            && text.is_char_boundary(span.range.start)
            && text.is_char_boundary(span.range.end)
    });
    spans.sort_by_key(|span| span.range.start);
    let mut cursor = 0usize;
    spans.retain_mut(|span| {
        if span.range.start < cursor {
            span.range.start = cursor;
        }
        if span.range.start >= span.range.end || !text.is_char_boundary(span.range.start) {
            return false;
        }
        cursor = span.range.end;
        true
    });
    spans
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let multi_line = self.input.read(cx).multi_line;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if multi_line {
            let input = self.input.clone();
            let layout_id = window.request_measured_layout(style, {
                move |known_dimensions, available_space, window, cx| {
                    let width = known_dimensions.width.or(match available_space.width {
                        gpui::AvailableSpace::Definite(width) => Some(width),
                        _ => None,
                    });
                    let element = TextElement {
                        input: input.clone(),
                    };
                    let (lines, line_height) = element.shape(width, window, cx);
                    let height: Pixels = lines
                        .iter()
                        .map(|l| l.size(line_height).height)
                        .fold(px(0.0), |a, b| a + b)
                        .max(line_height);
                    size(width.unwrap_or(px(0.0)), height)
                }
            });
            (layout_id, ())
        } else {
            style.size.height = window.line_height().into();
            (window.request_layout(style, [], cx), ())
        }
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (lines, line_height) = self.shape(Some(bounds.size.width), window, cx);

        // Cursor.
        let input = self.input.read(cx);
        let cursor_color = input.style.cursor_color;
        let selected_range = input.selected_range.clone();
        let cursor_offset = input.cursor_offset();
        let content_empty = input.content.is_empty();
        let multi_line = input.multi_line;
        let mut scroll_offset = input.scroll_offset;

        // Text-local cursor position across wrapped lines.
        let cursor_pos = if content_empty {
            Some(point(px(0.0), px(0.0)))
        } else {
            position_for_offset(&lines, line_height, cursor_offset)
        };

        // Keep the caret visible in single-line fields by scrolling the text
        // horizontally.
        if multi_line {
            scroll_offset = px(0.0);
        } else if let Some(pos) = cursor_pos {
            let text_width = lines.first().map_or(px(0.0), |line| line.width());
            scroll_offset =
                horizontal_scroll_offset(scroll_offset, pos.x, text_width, bounds.size.width);
        }
        self.input
            .update(cx, |input, _| input.scroll_offset = scroll_offset);

        let mut cursor = None;
        if selected_range.is_empty()
            && let Some(pos) = cursor_pos
        {
            cursor = Some(fill(
                Bounds::new(
                    point(bounds.left() + pos.x - scroll_offset, bounds.top() + pos.y),
                    size(px(1.5), line_height),
                ),
                cursor_color,
            ));
        }

        PrepaintState {
            lines,
            line_height,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        let line_height = prepaint.line_height;
        let scroll_offset = self.input.read(cx).scroll_offset;
        // Clip: scrolled single-line text must not bleed past the field.
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            let mut origin = point(bounds.origin.x - scroll_offset, bounds.origin.y);
            for line in prepaint.lines.iter() {
                let _ = line.paint_background(
                    origin,
                    line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                let _ = line.paint(origin, line_height, gpui::TextAlign::Left, None, window, cx);
                origin.y += line.size(line_height).height;
            }

            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });

        let lines = prepaint.lines.clone();
        self.input.update(cx, |input, _cx| {
            input.last_lines = lines;
            input.last_line_height = line_height;
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx).tab_stop(!self.disabled))
            .cursor(if self.disabled {
                CursorStyle::Arrow
            } else {
                CursorStyle::IBeam
            })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_to_home))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .text_size(px(self.style.font_size))
            .text_color(self.style.text_color)
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn shape_key(text: &str) -> ShapeKey {
        ShapeKey {
            text: text.to_owned().into(),
            is_placeholder: false,
            font: gpui::Font::default(),
            style: InputStyle::from_palette(Palette::default(), 14.0),
            selection: 0..0,
            marked: None,
            spans: Vec::new(),
            wrap: Some(px(200.0)),
            scale_factor: 1.0,
        }
    }

    #[test]
    fn shaping_ignores_caret_motion_but_tracks_selection_and_external_highlights() {
        let original = shape_key("hello world");
        let mut changed = shape_key("hello world");
        changed.selection = ShapeKey::selection(&(5..5), false);
        assert!(original == changed);
        assert_eq!(text_runs(&changed).len(), 1);

        changed.selection = ShapeKey::selection(&(0..5), false);
        assert!(original != changed);
        let runs = text_runs(&changed);
        assert!(runs[0].background_color.is_some());
        assert!(runs[1].background_color.is_none());

        changed.selection = 0..0;
        changed.spans = vec![Span::new(0..5).bold()];
        assert!(original != changed);
        assert_eq!(text_runs(&changed)[0].font.weight, gpui::FontWeight::BOLD);

        changed.spans.clear();
        changed.marked = Some(0..5);
        assert!(original != changed);
        assert!(text_runs(&changed)[0].underline.is_some());
    }

    #[test]
    fn shaping_tracks_content_placeholder_theme_font_width_and_scale() {
        let original = shape_key("hello");
        let changes: [fn(&mut ShapeKey); 8] = [
            |key: &mut ShapeKey| key.text = "world".into(),
            |key: &mut ShapeKey| key.is_placeholder = true,
            |key: &mut ShapeKey| key.style.font_size = 20.0,
            |key: &mut ShapeKey| {
                key.style = InputStyle::from_palette(Palette::from_hue(30.0, true), 14.0)
            },
            |key: &mut ShapeKey| key.font = gpui::font("monospace"),
            |key: &mut ShapeKey| key.wrap = Some(px(100.0)),
            |key: &mut ShapeKey| key.wrap = None,
            |key: &mut ShapeKey| key.scale_factor = 2.0,
        ];
        for change in changes {
            let mut changed = shape_key("hello");
            change(&mut changed);
            assert!(original != changed);
        }
    }

    #[test]
    fn highlight_run_walk_preserves_gaps_selection_and_ime_overrides() {
        let mut key = shape_key("abcdefghi");
        key.spans = vec![
            Span::new(1..3).bold().background(key.style.accent_color),
            Span::new(5..8).italic().squiggle(key.style.accent_color),
        ];
        key.selection = 2..6;
        key.marked = Some(6..7);
        let runs = text_runs(&key);
        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            key.text.len()
        );
        let mut offset = 0;
        for run in runs {
            for byte in offset..offset + run.len {
                assert_eq!(
                    run.font.weight == gpui::FontWeight::BOLD,
                    (1..3).contains(&byte)
                );
                assert_eq!(
                    run.font.style == gpui::FontStyle::Italic,
                    (5..8).contains(&byte)
                );
                if (2..6).contains(&byte) {
                    assert_eq!(run.background_color.unwrap().alpha, 0.45);
                } else {
                    assert_eq!(run.background_color.is_some(), byte == 1);
                }
                if byte == 6 {
                    assert!(!run.underline.unwrap().wavy);
                } else if (5..8).contains(&byte) {
                    assert!(run.underline.unwrap().wavy);
                } else {
                    assert!(run.underline.is_none());
                }
            }
            offset += run.len;
        }
    }

    #[test]
    fn replacement_preserves_unicode_at_the_start_middle_and_end() {
        for (text, range, replacement, expected) in [
            ("a😀éz", 1..7, "日本", "a日本z"),
            ("text", 0..0, "prefix ", "prefix text"),
            ("text", 4..4, " suffix", "text suffix"),
            ("text", 0..4, "", ""),
        ] {
            let result = replacing_text(text, range, replacement);
            assert_eq!(result, expected);
        }
    }

    fn line(text: &str, wrap_at: Option<usize>) -> gpui::WrappedLine {
        let glyphs: Vec<_> = text
            .char_indices()
            .enumerate()
            .map(|(column, (index, _))| gpui::ShapedGlyph {
                id: gpui::GlyphId(0),
                position: point(px(column as f32 * 10.0), px(0.0)),
                index,
                is_emoji: false,
            })
            .collect();
        let mut line = gpui::WrappedLine::default();
        line.text = text.to_string().into();
        *line = Arc::new(gpui::WrappedLineLayout {
            unwrapped_layout: Arc::new(gpui::LineLayout {
                width: px(glyphs.len() as f32 * 10.0),
                len: text.len(),
                runs: vec![gpui::ShapedRun {
                    font_id: gpui::FontId(0),
                    glyphs,
                }],
                ..Default::default()
            }),
            wrap_boundaries: wrap_at
                .map(|glyph_ix| gpui::WrapBoundary {
                    run_ix: 0,
                    glyph_ix,
                })
                .into_iter()
                .collect(),
            wrap_width: wrap_at.map(|column| px(column as f32 * 10.0)),
        });
        line
    }

    #[test]
    fn double_click_selects_the_word_whitespace_or_punctuation_under_it() {
        let text = "hello  world!";
        assert_eq!(word_range_at(text, 2), 0..5);
        assert_eq!(word_range_at(text, 5), 5..7);
        assert_eq!(word_range_at(text, 6), 5..7);
        assert_eq!(word_range_at(text, 7), 7..12);
        assert_eq!(word_range_at(text, 12), 12..13);
        assert_eq!(word_range_at("", 0), 0..0);

        let text = "café déjà";
        assert_eq!(word_range_at(text, 3), 0..5);
        assert_eq!(word_range_at(text, 8), 6..12);
        assert_eq!(word_range_at(text, text.len()), 6..12);
    }

    #[test]
    fn utf16_offsets_handle_surrogates_and_clamp_to_the_text() {
        let text = "a😀é";
        for (utf16, utf8) in [(0, 0), (1, 1), (2, 5), (3, 5), (4, 7), (usize::MAX, 7)] {
            assert_eq!(utf8_offset_for_utf16(text, utf16), utf8);
        }
    }

    #[test]
    fn empty_input_never_selects_its_multiline_placeholder() {
        let lines = [line("placeholder", Some(5)), line("second line", None)];
        for y in [-20.0, 10.0, 30.0, 50.0, 100.0] {
            assert_eq!(
                offset_for_position("", &lines, px(20.0), point(px(80.0), px(y))),
                0
            );
        }
    }

    #[test]
    fn hit_testing_clamps_stale_layout_to_valid_content_boundaries() {
        let lines = [line("stale layout", None)];
        assert_eq!(
            offset_for_position("é", &lines, px(20.0), point(px(10.0), px(10.0))),
            0
        );
        assert_eq!(
            offset_for_position("é", &lines, px(20.0), point(px(80.0), px(10.0))),
            2
        );
    }

    #[test]
    fn hit_testing_and_caret_positions_account_for_wrapping_and_newlines() {
        let content = "abcdef\néz";
        let lines = [line("abcdef", Some(3)), line("éz", None)];
        assert_eq!(
            offset_for_position(content, &lines, px(20.0), point(px(10.0), px(30.0))),
            4
        );
        assert_eq!(
            offset_for_position(content, &lines, px(20.0), point(px(10.0), px(50.0))),
            9
        );
        assert_eq!(
            position_for_offset(&lines, px(20.0), 4),
            Some(point(px(10.0), px(20.0)))
        );
        assert_eq!(
            position_for_offset(&lines, px(20.0), 9),
            Some(point(px(10.0), px(40.0)))
        );
        assert_eq!(
            offset_for_position(content, &lines, px(20.0), point(px(0.0), px(80.0))),
            content.len()
        );
        assert_eq!(
            offset_for_position(content, &lines, px(20.0), point(px(80.0), px(-1.0))),
            0
        );
    }

    #[test]
    fn scrolling_keeps_the_caret_visible_and_releases_unused_space() {
        for (previous, cursor, text, viewport, expected) in [
            (0.0, 180.0, 200.0, 100.0, 82.0),
            (82.0, 20.0, 200.0, 100.0, 20.0),
            (82.0, 90.0, 90.0, 100.0, 0.0),
            (82.0, 180.0, 200.0, 300.0, 0.0),
            (0.0, 20.0, 200.0, 1.0, 20.0),
        ] {
            assert_eq!(
                horizontal_scroll_offset(px(previous), px(cursor), px(text), px(viewport)),
                px(expected)
            );
        }
    }

    #[test]
    fn ime_bounds_follow_horizontal_scrolling_and_the_first_visual_row() {
        let bounds = Bounds::new(point(px(100.0), px(200.0)), size(px(100.0), px(100.0)));
        let scrolled = first_rect_for_range(
            bounds,
            point(px(80.0), px(0.0)),
            point(px(90.0), px(0.0)),
            px(20.0),
            px(60.0),
        );
        assert_eq!(
            scrolled,
            Bounds::new(point(px(120.0), px(200.0)), size(px(10.0), px(20.0)))
        );
        let wrapped = first_rect_for_range(
            bounds,
            point(px(80.0), px(20.0)),
            point(px(10.0), px(60.0)),
            px(20.0),
            px(0.0),
        );
        assert_eq!(
            wrapped,
            Bounds::new(point(px(180.0), px(220.0)), size(px(20.0), px(20.0)))
        );
    }
}
