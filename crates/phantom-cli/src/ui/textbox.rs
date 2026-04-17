//! Multi-line text input widget for interactive CLI prompts.
//!
//! Renders a bordered textbox in the terminal where users can type
//! multi-line text. Enter confirms, Escape cancels, Alt+Enter inserts
//! a newline. Long lines soft-wrap to fit the terminal width.

use dialoguer::console::{Key, Term, style};

const BOX_HEIGHT: usize = 8;
const MIN_WIDTH: usize = 40;

/// Editor state, separated from terminal I/O for testability.
struct EditorState {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    /// Scroll offset in visual (wrapped) rows.
    scroll_offset: usize,
    /// Content width available for text (terminal width minus padding).
    inner_width: usize,
}

impl EditorState {
    fn new(inner_width: usize) -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
            inner_width: inner_width.max(1),
        }
    }

    fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        if self.cursor_col >= line.len() {
            line.push(c);
        } else {
            line.insert(self.cursor_col, c);
        }
        self.cursor_col += c.len_utf8();
    }

    fn insert_newline(&mut self) {
        let tail = self.lines[self.cursor_row][self.cursor_col..].to_string();
        self.lines[self.cursor_row].truncate(self.cursor_col);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, tail);
        self.cursor_col = 0;
        self.ensure_cursor_visible();
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            // Find the previous char boundary.
            let prev = floor_char_boundary(line, self.cursor_col - 1);
            line.remove(prev);
            self.cursor_col = prev;
        } else if self.cursor_row > 0 {
            let removed = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&removed);
            self.ensure_cursor_visible();
        }
    }

    fn delete(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.lines[self.cursor_row].remove(self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            let line = &self.lines[self.cursor_row];
            self.cursor_col = floor_char_boundary(line, self.cursor_col - 1);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.ensure_cursor_visible();
        }
    }

    fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            let line = &self.lines[self.cursor_row];
            self.cursor_col = ceil_char_boundary(line, self.cursor_col + 1);
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
            self.ensure_cursor_visible();
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.clamp_cursor_col();
            self.ensure_cursor_visible();
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.clamp_cursor_col();
            self.ensure_cursor_visible();
        }
    }

    fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    fn clamp_cursor_col(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col > line_len {
            self.cursor_col = line_len;
        }
    }

    /// Number of visual (wrapped) rows a logical line occupies.
    fn visual_rows_for_line(&self, line_idx: usize) -> usize {
        let len = self.lines[line_idx].len();
        if len == 0 {
            return 1;
        }
        len.div_ceil(self.inner_width)
    }

    /// Absolute visual row index of the cursor across all lines.
    fn cursor_visual_row(&self) -> usize {
        let mut vrow = 0;
        for i in 0..self.cursor_row {
            vrow += self.visual_rows_for_line(i);
        }
        vrow + self.cursor_col / self.inner_width
    }

    /// Column within the current wrapped visual line.
    fn cursor_visual_col(&self) -> usize {
        self.cursor_col % self.inner_width
    }

    fn ensure_cursor_visible(&mut self) {
        let cursor_vrow = self.cursor_visual_row();
        if cursor_vrow < self.scroll_offset {
            self.scroll_offset = cursor_vrow;
        } else if cursor_vrow >= self.scroll_offset + BOX_HEIGHT {
            self.scroll_offset = cursor_vrow - BOX_HEIGHT + 1;
        }
    }
}

impl std::fmt::Display for EditorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lines.join("\n");
        f.write_str(&s)
    }
}

/// Find the largest byte index <= `idx` that is a char boundary.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Find the smallest byte index >= `idx` that is a char boundary.
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i.min(s.len())
}

/// RAII guard that restores terminal state on drop.
struct TermGuard<'a> {
    term: &'a Term,
    lines_rendered: usize,
}

impl Drop for TermGuard<'_> {
    fn drop(&mut self) {
        let _ = self.term.show_cursor();
        if self.lines_rendered > 0 {
            // Cursor is on the last rendered line. Move to the first and clear.
            if self.lines_rendered > 1 {
                let _ = self.term.move_cursor_up(self.lines_rendered - 1);
            }
            let _ = self.term.write_str("\r");
            let _ = self.term.clear_to_end_of_screen();
        }
    }
}

/// Show an interactive multi-line text input with horizontal separator lines.
///
/// The textbox spans the full terminal width. Only horizontal lines are drawn
/// (no vertical borders). Previous terminal output is preserved. Long lines
/// soft-wrap at the terminal boundary and respond to terminal resizes.
///
/// Returns `Some(text)` on confirmation (Enter), `None` on cancel (Escape/Ctrl+C).
#[allow(unused_assignments)]
pub fn multiline_input(title: &str, placeholder: &str) -> anyhow::Result<Option<String>> {
    let term = Term::stderr();
    if !term.features().is_attended() {
        anyhow::bail!("interactive input requires a terminal");
    }

    let (_, term_width) = term.size();
    let width = (term_width as usize).max(MIN_WIDTH);
    let inner_width = width.saturating_sub(2);

    let mut state = EditorState::new(inner_width);
    let mut guard = TermGuard {
        term: &term,
        lines_rendered: 0,
    };

    let mut first_render = true;

    loop {
        // Re-read terminal width for responsive wrapping on resize.
        let (_, term_width) = term.size();
        let width = (term_width as usize).max(MIN_WIDTH);
        let inner_width = width.saturating_sub(2);
        state.inner_width = inner_width.max(1);

        // On subsequent renders, move cursor back to start of widget and clear.
        if !first_render {
            // We left the cursor on the hint line (last rendered line).
            // Move up to the first rendered line, then clear everything below.
            let total_lines = guard.lines_rendered;
            if total_lines > 1 {
                term.move_cursor_up(total_lines - 1)?;
            }
            term.write_str("\r")?;
            term.clear_to_end_of_screen()?;
        }
        first_render = false;

        // Render the widget.
        let rendered = render(&state, title, placeholder, width);
        // Write all lines except the last with write_line (adds \n),
        // write the last with write_str (no trailing \n).
        // This leaves the cursor on the last rendered line.
        for (i, line) in rendered.iter().enumerate() {
            if i < rendered.len() - 1 {
                term.write_line(line)?;
            } else {
                term.write_str(line)?;
            }
        }
        guard.lines_rendered = rendered.len();

        // Position cursor inside the editing area.
        // Cursor is currently on the last line (hint). Content rows start
        // at line index 2 (after title + top separator). The target row is
        // 2 + visible_row (where visible_row is relative to scroll_offset).
        let cursor_vrow = state.cursor_visual_row();
        let visible_row = cursor_vrow - state.scroll_offset;
        let target_line = 2 + visible_row;
        let current_line = rendered.len() - 1;
        let lines_up = current_line - target_line;
        if lines_up > 0 {
            term.move_cursor_up(lines_up)?;
        }
        // Move cursor to column: 2 spaces left padding + visual column.
        term.write_str("\r")?;
        let col = 2 + state.cursor_visual_col();
        if col > 0 {
            term.move_cursor_right(col)?;
        }
        term.show_cursor()?;
        term.flush()?;

        // Read input.
        let key = term.read_key()?;
        term.hide_cursor()?;

        // After read_key, move cursor back to the last line (hint) so the
        // next clear cycle starts from a known position.
        if lines_up > 0 {
            term.move_cursor_down(lines_up)?;
        }
        term.write_str("\r")?;

        match key {
            Key::Enter => {
                let result = state.to_string();
                return Ok(Some(result));
            }
            Key::Escape | Key::CtrlC => {
                return Ok(None);
            }
            Key::UnknownEscSeq(ref seq) if is_newline_combo(seq) => {
                // Shift+Enter or Alt+Enter -> insert newline.
                state.insert_newline();
            }
            Key::Backspace => state.backspace(),
            Key::Del => state.delete(),
            Key::ArrowLeft => state.move_left(),
            Key::ArrowRight => state.move_right(),
            Key::ArrowUp => state.move_up(),
            Key::ArrowDown => state.move_down(),
            Key::Home => state.move_home(),
            Key::End => state.move_end(),
            Key::Char(c) => state.insert_char(c),
            _ => {}
        }
    }
}

/// Build the flat list of visual (wrapped) rows from all logical lines.
///
/// Each logical line is split into chunks of `state.inner_width` bytes
/// (respecting char boundaries). An extra empty row is appended for the
/// cursor line when the cursor sits at a position that would start a new
/// visual row beyond the content (e.g. cursor at the end of a line whose
/// length is an exact multiple of `inner_width`).
fn build_visual_rows(state: &EditorState) -> Vec<String> {
    let w = state.inner_width;
    let mut rows = Vec::new();

    for (i, line) in state.lines.iter().enumerate() {
        if line.is_empty() {
            rows.push(String::new());
        } else {
            let mut start = 0;
            while start < line.len() {
                let mut end = (start + w).min(line.len());
                // Snap back to a char boundary if we landed in the middle of one.
                while end > start && !line.is_char_boundary(end) {
                    end -= 1;
                }
                rows.push(line[start..end].to_string());
                start = end;
            }
        }

        // If the cursor is on this line and would occupy a visual sub-row
        // past the last content chunk, add an empty row so the cursor has
        // somewhere to render.
        if i == state.cursor_row {
            let content_vrows = if line.is_empty() {
                1
            } else {
                line.len().div_ceil(w)
            };
            let cursor_sub_row = state.cursor_col / w;
            if cursor_sub_row >= content_vrows {
                rows.push(String::new());
            }
        }
    }

    rows
}

/// Render the textbox widget as a list of lines to print.
///
/// Layout: title, horizontal separator, content rows (soft-wrapped),
/// horizontal separator, hint. No vertical borders — content is
/// left-padded with 2 spaces.
fn render(state: &EditorState, title: &str, placeholder: &str, width: usize) -> Vec<String> {
    let mut output = Vec::new();

    // Title line.
    output.push(format!("  {}", style(title).bold()));

    // Top separator — full terminal width.
    output.push(format!("  {}", style("\u{2500}".repeat(width - 2)).dim()));

    if state.is_empty() {
        // Show placeholder on the first row, rest empty.
        let ph = truncate_str(placeholder, state.inner_width);
        output.push(format!("  {}", style(ph).dim()));
        for _ in 1..BOX_HEIGHT {
            output.push(String::new());
        }
    } else {
        // Build wrapped visual rows and show the visible slice.
        let visual_rows = build_visual_rows(state);
        let visible_end = (state.scroll_offset + BOX_HEIGHT).min(visual_rows.len());
        for row in 0..BOX_HEIGHT {
            let vrow_idx = state.scroll_offset + row;
            if vrow_idx < visible_end {
                output.push(format!("  {}", visual_rows[vrow_idx]));
            } else {
                output.push(String::new());
            }
        }
    }

    // Bottom separator — full terminal width.
    output.push(format!("  {}", style("\u{2500}".repeat(width - 2)).dim()));

    // Hint line.
    output.push(format!(
        "  {}",
        style("Enter to confirm \u{00b7} Shift+Enter for newline \u{00b7} Esc to cancel").dim()
    ));

    output
}

/// Check if an escape sequence represents Shift+Enter or Alt+Enter.
///
/// Different terminals send different sequences:
/// - Alt+Enter: ESC followed by CR (`\r`) or LF (`\n`)
/// - Shift+Enter (kitty protocol): `\x1b[13;2u` -> parsed as `['[','1','3',';','2','u']`
/// - Shift+Enter (xterm modifyOtherKeys): various sequences containing `13` or CR/LF
fn is_newline_combo(seq: &[char]) -> bool {
    // Alt+Enter: sequence contains CR or LF
    if seq.contains(&'\r') || seq.contains(&'\n') {
        return true;
    }
    // Kitty keyboard protocol: [13;2u means Shift+Enter
    let s: String = seq.iter().collect();
    if s.contains("13;2") {
        return true;
    }
    false
}

/// Truncate a string to fit within `max_width` display columns.
fn truncate_str(s: &str, max_width: usize) -> &str {
    if s.len() <= max_width {
        return s;
    }
    // Find the largest byte index that keeps us within max_width.
    let mut end = max_width;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default inner width for tests.
    const TEST_WIDTH: usize = 80;

    #[test]
    fn insert_char_at_end() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.insert_char('a');
        state.insert_char('b');
        assert_eq!(state.lines[0], "ab");
        assert_eq!(state.cursor_col, 2);
    }

    #[test]
    fn insert_char_in_middle() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.insert_char('a');
        state.insert_char('c');
        state.cursor_col = 1;
        state.insert_char('b');
        assert_eq!(state.lines[0], "abc");
        assert_eq!(state.cursor_col, 2);
    }

    #[test]
    fn backspace_at_start_joins_lines() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["hello".into(), "world".into()];
        state.cursor_row = 1;
        state.cursor_col = 0;
        state.backspace();
        assert_eq!(state.lines, vec!["helloworld"]);
        assert_eq!(state.cursor_row, 0);
        assert_eq!(state.cursor_col, 5);
    }

    #[test]
    fn backspace_deletes_char() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["abc".into()];
        state.cursor_col = 2;
        state.backspace();
        assert_eq!(state.lines[0], "ac");
        assert_eq!(state.cursor_col, 1);
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["helloworld".into()];
        state.cursor_col = 5;
        state.insert_newline();
        assert_eq!(state.lines, vec!["hello", "world"]);
        assert_eq!(state.cursor_row, 1);
        assert_eq!(state.cursor_col, 0);
    }

    #[test]
    fn delete_joins_with_next_line() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["hello".into(), "world".into()];
        state.cursor_col = 5; // at end of first line
        state.delete();
        assert_eq!(state.lines, vec!["helloworld"]);
    }

    #[test]
    fn move_right_wraps_to_next_line() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["ab".into(), "cd".into()];
        state.cursor_col = 2; // at end of first line
        state.move_right();
        assert_eq!(state.cursor_row, 1);
        assert_eq!(state.cursor_col, 0);
    }

    #[test]
    fn move_left_wraps_to_previous_line() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["ab".into(), "cd".into()];
        state.cursor_row = 1;
        state.cursor_col = 0;
        state.move_left();
        assert_eq!(state.cursor_row, 0);
        assert_eq!(state.cursor_col, 2);
    }

    #[test]
    fn scroll_offset_adjusts_when_cursor_below_view() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = (0..20).map(|i| format!("line {i}")).collect();
        state.cursor_row = 0;
        state.scroll_offset = 0;
        // Move cursor beyond visible area.
        state.cursor_row = 15;
        state.ensure_cursor_visible();
        assert!(state.scroll_offset > 0);
        let cursor_vrow = state.cursor_visual_row();
        assert!(cursor_vrow < state.scroll_offset + BOX_HEIGHT);
    }

    #[test]
    fn to_string_joins_lines() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["hello".into(), "world".into()];
        assert_eq!(state.to_string(), "hello\nworld");
    }

    #[test]
    fn is_empty_when_single_empty_line() {
        let state = EditorState::new(TEST_WIDTH);
        assert!(state.is_empty());
    }

    #[test]
    fn is_not_empty_after_typing() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.insert_char('a');
        assert!(!state.is_empty());
    }

    #[test]
    fn move_up_clamps_cursor_col() {
        let mut state = EditorState::new(TEST_WIDTH);
        state.lines = vec!["hi".into(), "hello world".into()];
        state.cursor_row = 1;
        state.cursor_col = 10;
        state.move_up();
        assert_eq!(state.cursor_row, 0);
        assert_eq!(state.cursor_col, 2); // clamped to "hi".len()
    }

    // ── Wrapping tests ────────────────────────────────────────────────

    #[test]
    fn visual_rows_for_short_line() {
        let mut state = EditorState::new(10);
        state.lines = vec!["hello".into()];
        assert_eq!(state.visual_rows_for_line(0), 1);
    }

    #[test]
    fn visual_rows_for_exact_width_line() {
        let mut state = EditorState::new(5);
        state.lines = vec!["hello".into()]; // exactly 5 chars, width 5
        assert_eq!(state.visual_rows_for_line(0), 1);
    }

    #[test]
    fn visual_rows_for_long_line() {
        let mut state = EditorState::new(5);
        state.lines = vec!["hello world!".into()]; // 12 chars, width 5
        assert_eq!(state.visual_rows_for_line(0), 3); // ceil(12/5) = 3
    }

    #[test]
    fn visual_rows_for_empty_line() {
        let mut state = EditorState::new(10);
        state.lines = vec![String::new()];
        assert_eq!(state.visual_rows_for_line(0), 1);
    }

    #[test]
    fn cursor_visual_row_on_first_line() {
        let mut state = EditorState::new(5);
        state.lines = vec!["hello world!".into()];
        state.cursor_col = 0;
        assert_eq!(state.cursor_visual_row(), 0);
        state.cursor_col = 4;
        assert_eq!(state.cursor_visual_row(), 0);
        state.cursor_col = 5;
        assert_eq!(state.cursor_visual_row(), 1);
        state.cursor_col = 10;
        assert_eq!(state.cursor_visual_row(), 2);
    }

    #[test]
    fn cursor_visual_row_across_lines() {
        let mut state = EditorState::new(5);
        // Line 0: "hello world!" = 12 chars = 3 visual rows
        // Line 1: "ab" = 1 visual row
        state.lines = vec!["hello world!".into(), "ab".into()];
        state.cursor_row = 1;
        state.cursor_col = 0;
        assert_eq!(state.cursor_visual_row(), 3); // 3 rows from line 0
    }

    #[test]
    fn cursor_visual_col_wraps() {
        let mut state = EditorState::new(5);
        state.lines = vec!["hello world!".into()];
        state.cursor_col = 7; // 7 % 5 = 2
        assert_eq!(state.cursor_visual_col(), 2);
    }

    #[test]
    fn build_visual_rows_wraps_long_line() {
        let mut state = EditorState::new(5);
        state.lines = vec!["abcdefghij".into()]; // 10 chars, width 5
        state.cursor_row = 0;
        state.cursor_col = 0;
        let rows = build_visual_rows(&state);
        assert_eq!(rows, vec!["abcde", "fghij"]);
    }

    #[test]
    fn build_visual_rows_adds_cursor_row_at_exact_boundary() {
        let mut state = EditorState::new(5);
        state.lines = vec!["abcde".into()]; // 5 chars, width 5 -> 1 visual row
        state.cursor_row = 0;
        state.cursor_col = 5; // cursor at end, visual sub-row = 1
        let rows = build_visual_rows(&state);
        // Should have content row + extra empty row for cursor
        assert_eq!(rows, vec!["abcde", ""]);
    }

    #[test]
    fn scroll_adjusts_for_wrapped_lines() {
        let mut state = EditorState::new(5);
        // A single line that wraps to 20 visual rows (100 chars / 5 width)
        state.lines = vec!["a".repeat(100)];
        state.cursor_col = 95; // visual row 19
        state.ensure_cursor_visible();
        let cursor_vrow = state.cursor_visual_row();
        assert!(cursor_vrow >= state.scroll_offset);
        assert!(cursor_vrow < state.scroll_offset + BOX_HEIGHT);
    }

    #[test]
    fn render_wraps_instead_of_truncating() {
        let mut state = EditorState::new(10);
        state.lines = vec!["abcdefghijklmno".into()]; // 15 chars, width 10
        state.cursor_row = 0;
        state.cursor_col = 0;
        let rendered = render(&state, "Title", "placeholder", 12);
        // Lines 2 and 3 should contain the wrapped content
        assert!(rendered[2].contains("abcdefghij"));
        assert!(rendered[3].contains("klmno"));
    }
}
