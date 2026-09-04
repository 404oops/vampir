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

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Rgba, SharedString,
    Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*,
    px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

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
        }
    }

    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
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
#[derive(Clone, Copy)]
pub struct InputStyle {
    pub text_color: Rgba,
    pub placeholder_color: Rgba,
    pub selection_color: Rgba,
    pub cursor_color: Rgba,
    pub font_size: f32,
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
    last_lines: Vec<gpui::WrappedLine>,
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
            placeholder: placeholder.to_string().into(),
            multi_line,
            style,
            disabled: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
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

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_string().into();
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
            let text = self.content.to_string();
            on_change(&text, cx);
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
        if let Some(index) = self.offset_for_position_local(target) {
            if select {
                self.select_to(index, cx);
            } else {
                self.move_to(index, cx);
            }
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
        if self.multi_line {
            self.replace_text_in_range(None, "\n", window, cx);
        } else if let Some(on_submit) = self.on_submit.take() {
            let text = self.content.to_string();
            on_submit(&text, cx);
            self.on_submit = Some(on_submit);
        } else {
            // Nothing to submit to: the Enter belongs to whatever holds the
            // field. See `up`.
            cx.propagate();
        }
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
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
                // Double-click selects the word under the cursor.
                let start = self.previous_word_boundary(self.next_boundary(index).min(index + 1));
                let end = self.next_word_boundary(index);
                self.move_to(start.min(end), cx);
                self.select_to(end.max(start), cx);
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
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
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
        let line_height = self.last_line_height;
        let mut y_offset = px(0.0);
        let mut start = 0usize;
        for line in &self.last_lines {
            let line_len = line.text.len();
            let end = start + line_len;
            if offset <= end {
                let local = line.position_for_index(offset - start, line_height)?;
                return Some(point(local.x, local.y + y_offset));
            }
            y_offset += line.size(line_height).height;
            start = end + 1; // skip the '\n'
        }
        None
    }

    /// Offset for a point in text-local coordinates.
    fn offset_for_position_local(&self, position: Point<Pixels>) -> Option<usize> {
        let line_height = self.last_line_height;
        let mut y_offset = px(0.0);
        let mut start = 0usize;
        for (i, line) in self.last_lines.iter().enumerate() {
            let height = line.size(line_height).height;
            let local = point(position.x, position.y - y_offset);
            let within = position.y >= y_offset && position.y < y_offset + height;
            let is_last = i + 1 == self.last_lines.len();
            if within || (is_last && position.y >= y_offset) {
                let index = line
                    .closest_index_for_position(local, line_height)
                    .unwrap_or_else(|idx| idx);
                return Some(start + index.min(line.text.len()));
            }
            y_offset += height;
            start += line.text.len() + 1;
        }
        if position.y < px(0.0) {
            Some(0)
        } else {
            Some(self.content.len())
        }
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
            .unwrap_or(self.content.len())
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
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
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
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
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

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
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
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
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
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
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
        let line_height = self.last_line_height;
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + start.y),
            point(bounds.left() + end.x, bounds.top() + end.y + line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point_: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let local = point(point_.x - bounds.left(), point_.y - bounds.top());
        let utf8_index = self.offset_for_position_local(local)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    lines: Vec<gpui::WrappedLine>,
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
    ) -> (Vec<gpui::WrappedLine>, Pixels) {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let marked_range = input.marked_range.clone();
        let style = window.text_style();
        let input_style = input.style;
        let font_size = px(input_style.font_size);
        let multi_line = input.multi_line;

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), input_style.placeholder_color)
        } else {
            (content, input_style.text_color)
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

        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: crate::color::to_hsla(text_color),
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        };

        // Split into runs so the selection gets a background color, any
        // marked (IME composition) range gets an underline, and each
        // highlight span gets its own styling.
        let mut runs: Vec<TextRun> = Vec::new();
        if is_placeholder {
            runs.push(base_run.clone());
        } else {
            let mut boundaries: Vec<usize> = vec![0, display_text.len()];
            boundaries.push(selected_range.start.min(display_text.len()));
            boundaries.push(selected_range.end.min(display_text.len()));
            if let Some(marked) = &marked_range {
                boundaries.push(marked.start.min(display_text.len()));
                boundaries.push(marked.end.min(display_text.len()));
            }
            for span in &spans {
                boundaries.push(span.range.start);
                boundaries.push(span.range.end);
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            for pair in boundaries.windows(2) {
                let (start, end) = (pair[0], pair[1]);
                if end <= start {
                    continue;
                }
                let mut run = TextRun {
                    len: end - start,
                    ..base_run.clone()
                };
                if let Some(span) = spans
                    .iter()
                    .find(|span| span.range.start <= start && end <= span.range.end)
                {
                    if let Some(color) = span.color {
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
                if let Some(marked) = &marked_range
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
        }

        let wrap = if multi_line { wrap_width } else { None };
        let lines = window
            .text_system()
            .shape_text(display_text, font_size, &runs, wrap, None)
            .map(|lines| lines.into_vec())
            .unwrap_or_default();
        (lines, window.line_height())
    }
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
            let mut pos = None;
            let mut y_offset = px(0.0);
            let mut start = 0usize;
            for line in &lines {
                let end = start + line.text.len();
                if cursor_offset <= end {
                    if let Some(local) = line.position_for_index(cursor_offset - start, line_height)
                    {
                        pos = Some(point(local.x, local.y + y_offset));
                    }
                    break;
                }
                y_offset += line.size(line_height).height;
                start = end + 1;
            }
            pos
        };

        // Keep the caret visible in single-line fields by scrolling the text
        // horizontally.
        if multi_line {
            scroll_offset = px(0.0);
        } else if let Some(pos) = cursor_pos {
            let margin = px(2.0);
            let visible_width = (bounds.size.width - margin).max(px(0.0));
            if pos.x - scroll_offset > visible_width {
                scroll_offset = pos.x - visible_width;
            }
            if pos.x - scroll_offset < px(0.0) {
                scroll_offset = pos.x;
            }
            scroll_offset = scroll_offset.max(px(0.0));
        }
        self.input
            .update(cx, |input, _| input.scroll_offset = scroll_offset);

        let mut cursor = None;
        if selected_range.is_empty()
            && let Some(pos) = cursor_pos
        {
            {
                cursor = Some(fill(
                    Bounds::new(
                        point(bounds.left() + pos.x - scroll_offset, bounds.top() + pos.y),
                        size(px(1.5), line_height),
                    ),
                    cursor_color,
                ));
            }
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
            for line in &prepaint.lines {
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

        let lines = std::mem::take(&mut prepaint.lines);
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
            .track_focus(&self.focus_handle(cx))
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
