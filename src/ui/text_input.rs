//! A text field: single- or multi-line, with selection, clipboard, IME,
//! word motion, and an optional password mask. Built on GPUI's wrapped text
//! layout, since GPUI ships no input widget of its own.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, SharedString, Style, Subscription, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        Paste,
        Cut,
        Copy,
        Enter,
        Newline,
    ]
);

const CONTEXT: &str = "TextInput";

pub fn bind_keys(cx: &mut App) {
    let ctx = Some(CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        KeyBinding::new("secondary-a", SelectAll, ctx),
        KeyBinding::new("secondary-v", Paste, ctx),
        KeyBinding::new("secondary-c", Copy, ctx),
        KeyBinding::new("secondary-x", Cut, ctx),
        KeyBinding::new("enter", Enter, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
            KeyBinding::new("cmd-shift-left", SelectHome, ctx),
            KeyBinding::new("cmd-shift-right", SelectEnd, ctx),
            KeyBinding::new("cmd-up", DocStart, ctx),
            KeyBinding::new("cmd-down", DocEnd, ctx),
            KeyBinding::new("alt-left", WordLeft, ctx),
            KeyBinding::new("alt-right", WordRight, ctx),
            KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
            KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
            KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
            KeyBinding::new("ctrl-a", Home, ctx),
            KeyBinding::new("ctrl-e", End, ctx),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-left", WordLeft, ctx),
            KeyBinding::new("ctrl-right", WordRight, ctx),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
            KeyBinding::new("ctrl-home", DocStart, ctx),
            KeyBinding::new("ctrl-end", DocEnd, ctx),
        ]);
    }
    cx.bind_keys(bindings);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Changed,
    /// Enter in a single-line field, or Enter without Shift in a field with
    /// `submit_on_enter`.
    Submit,
    /// Focus left the field — settings fields save here, not per keystroke,
    /// so a save round-trip never steals the caret mid-word.
    Blur,
    Focus,
}

impl EventEmitter<InputEvent> for TextInput {}

/// One logical line (between '\n's), shaped and possibly wrapped.
struct LineLayout {
    /// Byte offset of the line's start in the displayed text.
    start: usize,
    line: WrappedLine,
    /// Top of this line relative to the text origin.
    y: Pixels,
    rows: usize,
}

struct Layout {
    lines: Vec<LineLayout>,
    line_height: Pixels,
    width: Pixels,
}

impl Layout {
    fn height(&self) -> Pixels {
        self.line_height * self.lines.iter().map(|l| l.rows).sum::<usize>().max(1) as f32
    }

    fn line_for_index(&self, ix: usize) -> &LineLayout {
        let mut found = &self.lines[0];
        for line in &self.lines {
            if line.start <= ix {
                found = line;
            } else {
                break;
            }
        }
        found
    }

    fn position_for_index(&self, ix: usize) -> Point<Pixels> {
        let line = self.line_for_index(ix);
        let local = (ix - line.start).min(line.line.len());
        let p = line.line.position_for_index(local, self.line_height).unwrap_or(point(px(0.), px(0.)));
        point(p.x, p.y + line.y)
    }

    fn index_for_position(&self, p: Point<Pixels>) -> usize {
        let mut chosen = &self.lines[0];
        for line in &self.lines {
            if p.y >= line.y {
                chosen = line;
            }
        }
        let local_y = (p.y - chosen.y).max(px(0.)).min(self.line_height * chosen.rows as f32 - px(0.5));
        let local = chosen
            .line
            .closest_index_for_position(point(p.x.max(px(0.)), local_y), self.line_height)
            .unwrap_or_else(|ix| ix);
        chosen.start + local.min(chosen.line.len())
    }
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    multiline: bool,
    submit_on_enter: bool,
    masked: bool,
    mono: bool,
    /// Rows shown before the field scrolls; `None` grows without limit.
    max_rows: Option<usize>,
    min_rows: usize,
    scroll_y: Pixels,
    /// Column to keep when moving up and down through lines of different length.
    goal_x: Option<Pixels>,
    last_layout: Option<Layout>,
    last_bounds: Option<Bounds<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

impl TextInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_blur(&focus_handle, window, |this, _, cx| {
                this.is_selecting = false;
                cx.emit(InputEvent::Blur);
                cx.notify();
            }),
            cx.on_focus(&focus_handle, window, |_, _, cx| {
                cx.emit(InputEvent::Focus);
                cx.notify();
            }),
        ];
        Self {
            focus_handle,
            content: String::new(),
            placeholder: SharedString::default(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            multiline: false,
            submit_on_enter: true,
            masked: false,
            mono: false,
            max_rows: None,
            min_rows: 1,
            scroll_y: px(0.),
            goal_x: None,
            last_layout: None,
            last_bounds: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Enter inserts a newline (Shift+Enter always does).
    pub fn multiline(mut self, rows: usize) -> Self {
        self.multiline = true;
        self.submit_on_enter = false;
        self.min_rows = rows.max(1);
        self
    }

    /// Multi-line, but Enter submits and Shift+Enter breaks the line.
    pub fn composer(mut self, max_rows: usize) -> Self {
        self.multiline = true;
        self.submit_on_enter = true;
        self.max_rows = Some(max_rows);
        self
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        let text = if self.multiline { text } else { text.replace('\n', " ") };
        if text == self.content {
            return;
        }
        self.content = text;
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_y = px(0.);
        cx.notify();
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        if text != self.placeholder {
            self.placeholder = text;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    /// Put the caret at the end, as focusing the ask box does.
    pub fn move_to_end(&mut self, cx: &mut Context<Self>) {
        let end = self.content.len();
        self.move_to(end, cx);
    }

    /* ---------- editing ---------- */

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content.unicode_word_indices().rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .find_map(|(idx, w)| (idx + w.len() > offset).then_some(idx + w.len()))
            .unwrap_or(self.content.len())
    }

    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.content[offset..].find('\n').map(|i| offset + i).unwrap_or(self.content.len())
    }

    /// Offset one visual row up or down, keeping the goal column.
    fn vertical(&mut self, rows: f32) -> Option<usize> {
        let layout = self.last_layout.as_ref()?;
        let cursor = self.display_offset(self.cursor_offset());
        let p = layout.position_for_index(cursor);
        let x = *self.goal_x.get_or_insert(p.x);
        let y = p.y + layout.line_height * rows + layout.line_height / 2.;
        if y < px(0.) {
            return Some(0);
        }
        if y > layout.height() {
            return Some(self.content.len());
        }
        let ix = layout.index_for_position(point(x, y));
        Some(self.content_offset(ix))
    }

    fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, text, window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            self.move_to(0, cx);
            return;
        }
        if let Some(ix) = self.vertical(-1.) {
            self.move_to(ix, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            self.move_to(self.content.len(), cx);
            return;
        }
        if let Some(ix) = self.vertical(1.) {
            self.move_to(ix, cx);
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.multiline { self.vertical(-1.) } else { Some(0) };
        if let Some(ix) = target {
            self.select_to(ix, cx);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.multiline { self.vertical(1.) } else { Some(self.content.len()) };
        if let Some(ix) = target {
            self.select_to(ix, cx);
        }
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.line_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.line_end(self.cursor_offset()), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(self.cursor_offset()), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(self.cursor_offset()), cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = if self.multiline { text } else { text.replace(['\r', '\n'], " ") };
            self.insert(&text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.insert("", window, cx);
        }
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && !self.submit_on_enter {
            self.insert("\n", window, cx);
        } else {
            cx.emit(InputEvent::Submit);
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.insert("\n", window, cx);
        } else {
            cx.emit(InputEvent::Submit);
        }
    }

    /* ---------- mouse ---------- */

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return self.content.len();
        };
        let local = point(position.x - bounds.left(), position.y - bounds.top() + self.scroll_y);
        self.content_offset(layout.index_for_position(local))
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A click in a field is not a click on whatever surrounds it.
        cx.stop_propagation();
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        self.goal_x = None;
        let ix = self.index_for_mouse(event.position);
        if event.modifiers.shift {
            self.select_to(ix, cx);
        } else if event.click_count >= 2 {
            // Double-click selects the word under the pointer.
            let start = self.previous_word(self.next_boundary(ix));
            let end = self.next_word(start);
            self.selected_range = start..end.max(start);
            self.selection_reversed = false;
            cx.notify();
        } else {
            self.move_to(ix, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse(event.position), cx);
        }
    }

    /* ---------- masking ---------- */

    /// What is painted: the content, or one bullet per character.
    fn display_text(&self) -> String {
        if self.masked { "•".repeat(self.content.chars().count()) } else { self.content.clone() }
    }

    fn display_offset(&self, content_offset: usize) -> usize {
        if !self.masked {
            return content_offset;
        }
        self.content[..content_offset.min(self.content.len())].chars().count() * '•'.len_utf8()
    }

    fn content_offset(&self, display_offset: usize) -> usize {
        if !self.masked {
            return display_offset.min(self.content.len());
        }
        let n = display_offset / '•'.len_utf8();
        self.content.char_indices().nth(n).map(|(i, _)| i).unwrap_or(self.content.len())
    }

    /* ---------- utf16 ---------- */

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn row_count(&self) -> usize {
        self.last_layout.as_ref().map(|l| l.lines.iter().map(|l| l.rows).sum()).unwrap_or(1)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = if self.multiline { new_text.to_string() } else { new_text.replace(['\r', '\n'], " ") };
        self.content.replace_range(range.clone(), &new_text);
        let caret = range.start + new_text.len();
        self.selected_range = caret..caret;
        self.selection_reversed = false;
        self.marked_range = None;
        self.changed(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content.replace_range(range.clone(), new_text);
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.changed(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_for_index(self.display_offset(range.start));
        let end = layout.position_for_index(self.display_offset(range.end));
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + start.y - self.scroll_y),
            point(bounds.left() + end.x, bounds.top() + end.y + layout.line_height - self.scroll_y),
        ))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.last_layout.as_ref()?;
        let local = point(p.x - bounds.left(), p.y - bounds.top() + self.scroll_y);
        let ix = self.content_offset(layout.index_for_position(local));
        Some(self.offset_to_utf16(ix))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .w_full()
            .overflow_hidden()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::newline))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

/* ---------- the element that lays out and paints the text ---------- */

struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    layout: Option<Layout>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    placeholder: Option<WrappedLine>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

fn shape(
    text: &str,
    font: gpui::Font,
    color: gpui::Hsla,
    marked: Option<Range<usize>>,
    width: Pixels,
    line_height: Pixels,
    font_size: Pixels,
    window: &mut Window,
) -> Layout {
    let mut lines = Vec::new();
    let mut y = px(0.);
    let mut start = 0;
    for logical in text.split('\n') {
        let len = logical.len();
        let base =
            TextRun { len, font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
        let runs = match &marked {
            Some(m) if m.start < start + len && m.end > start => {
                let a = m.start.saturating_sub(start).min(len);
                let b = (m.end - start).min(len);
                let underline = Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false });
                vec![
                    TextRun { len: a, ..base.clone() },
                    TextRun { len: b - a, underline, ..base.clone() },
                    TextRun { len: len - b, ..base },
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect()
            }
            _ => vec![base],
        };
        let shaped = window
            .text_system()
            .shape_text(SharedString::from(logical.to_string()), font_size, &runs, Some(width), None)
            .ok()
            .and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)))
            .unwrap_or_default();
        let rows = shaped.wrap_boundaries().len() + 1;
        lines.push(LineLayout { start, line: shaped, y, rows });
        y += line_height * rows as f32;
        start += len + 1;
    }
    Layout { lines, line_height, width }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let line_height = window.line_height();
        let rows = if input.multiline {
            let rows = input.row_count().max(input.min_rows);
            match input.max_rows {
                Some(max) => rows.min(max),
                None => rows,
            }
        } else {
            1
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (line_height * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let font = if input.mono { theme::mono_font() } else { style.font() };
        let width = if input.multiline { bounds.size.width } else { px(100_000.) };

        let text = input.display_text();
        let marked = input.marked_range.clone().map(|r| input.display_offset(r.start)..input.display_offset(r.end));
        let layout = shape(&text, font.clone(), style.color, marked, width, line_height, font_size, window);

        let placeholder = (text.is_empty() && !input.placeholder.is_empty()).then(|| {
            let run = TextRun {
                len: input.placeholder.len(),
                font: font.clone(),
                color: theme::hsla(theme::edge()),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window
                .text_system()
                .shape_text(input.placeholder.clone(), font_size, &[run], Some(bounds.size.width), Some(1))
                .ok()
                .and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)))
                .unwrap_or_default()
        });

        let sel = input.display_offset(input.selected_range.start)..input.display_offset(input.selected_range.end);
        let cursor_ix = input.display_offset(input.cursor_offset());
        let scroll_y = input.scroll_y;
        let origin = point(bounds.left(), bounds.top() - scroll_y);

        let mut selections = Vec::new();
        let selection_color = theme::hsla(theme::lamp()).opacity(0.28);
        if !sel.is_empty() {
            for line in &layout.lines {
                let line_end = line.start + line.line.len();
                let a = sel.start.max(line.start);
                let b = sel.end.min(line_end);
                // Include the newline itself when the selection runs past it.
                let past_end = sel.end > line_end;
                if a > b || (a == b && !past_end) {
                    continue;
                }
                let pa = layout.position_for_index(a);
                let pb = layout.position_for_index(b);
                let extra = if past_end { px(5.) } else { px(0.) };
                let mut row_y = pa.y;
                while row_y <= pb.y {
                    let x0 = if row_y == pa.y { pa.x } else { px(0.) };
                    let x1 = if row_y == pb.y { pb.x + extra } else { layout.width.min(bounds.size.width) };
                    selections.push(fill(
                        Bounds::from_corners(
                            point(origin.x + x0, origin.y + row_y),
                            point(origin.x + x1.max(x0 + px(2.)), origin.y + row_y + line_height),
                        ),
                        selection_color,
                    ));
                    row_y += line_height;
                }
            }
        }

        let cursor = sel.is_empty().then(|| {
            let p = layout.position_for_index(cursor_ix);
            fill(
                Bounds::new(point(origin.x + p.x, origin.y + p.y + px(2.)), size(px(1.5), line_height - px(4.))),
                theme::hsla(theme::lamp()),
            )
        });

        Prepaint { layout: Some(layout), cursor, selections, placeholder }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);

        let layout = prepaint.layout.take().expect("prepaint ran");
        let line_height = layout.line_height;
        let scroll_y = self.input.read(cx).scroll_y;

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.selections.drain(..) {
                window.paint_quad(quad);
            }
            if let Some(placeholder) = prepaint.placeholder.take() {
                let _ = placeholder.paint(bounds.origin, line_height, gpui::TextAlign::Left, Some(bounds), window, cx);
            }
            for line in &layout.lines {
                let origin = point(bounds.left(), bounds.top() + line.y - scroll_y);
                let _ = line.line.paint(origin, line_height, gpui::TextAlign::Left, Some(bounds), window, cx);
            }
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });

        // Keep the caret inside a field that scrolls (the ask box).
        let cursor_y = {
            let input = self.input.read(cx);
            layout.position_for_index(input.display_offset(input.cursor_offset())).y
        };
        let visible = bounds.size.height;
        let total = layout.height();
        self.input.update(cx, |input, cx| {
            let mut next = input.scroll_y;
            // Fields without a row limit grow to fit, so they never scroll.
            if input.max_rows.is_none() {
                next = px(0.);
            } else if cursor_y < next {
                next = cursor_y;
            } else if cursor_y + line_height > next + visible {
                next = cursor_y + line_height - visible;
            }
            next = next.min((total - visible).max(px(0.))).max(px(0.));
            let rows_changed = input.last_layout.as_ref().map(|l| l.lines.iter().map(|l| l.rows).sum::<usize>())
                != Some(layout.lines.iter().map(|l| l.rows).sum::<usize>());
            let scrolled = next != input.scroll_y;
            input.scroll_y = next;
            input.last_layout = Some(layout);
            input.last_bounds = Some(bounds);
            // Row count feeds the element height, which is decided before
            // layout — so a change needs one more frame.
            if rows_changed || scrolled {
                cx.notify();
            }
        });
    }
}
