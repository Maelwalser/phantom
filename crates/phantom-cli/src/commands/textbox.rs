//! Multi-line text input widget for interactive CLI prompts.
//!
//! Renders a bordered textbox in the terminal where users can type
//! multi-line text. Enter confirms, Escape cancels, Alt+Enter inserts
//! a newline.

use dialoguer::console::{style, Key, Term};

const BOX_HEIGHT: usize = 8;
const MIN_WIDTH: usize = 40;

/// Editor state, separated from terminal I/O for testability.
struct EditorState {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    scroll_offset: usize,
}

impl EditorState {
    fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
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

    fn ensure_cursor_visible(&mut self) {
        if self.cursor_row < self.scroll_offset {
            self.scroll_offset = self.cursor_row;
        } else if self.cursor_row >= self.scroll_offset + BOX_HEIGHT {
            self.scroll_offset = self.cursor_row - BOX_HEIGHT + 1;
        }
    }

    fn to_string(&self) -> String {
        self.lines.join("\n")
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
/// (no vertical borders). Previous terminal output is preserved.
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
    // Content area uses full width with a small left padding (2 spaces).
    let inner_width = width.saturating_sub(2);

    let mut state = EditorState::new();
    let mut guard = TermGuard {
        term: &term,
        lines_rendered: 0,
    };

    let mut first_render = true;

    loop {
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
        let rendered = render(&state, title, placeholder, width, inner_width);
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
        // 2 + visible_row. We need to go up from (rendered.len() - 1).
        let visible_row = state.cursor_row - state.scroll_offset;
        let target_line = 2 + visible_row;
        let current_line = rendered.len() - 1;
        let lines_up = current_line - target_line;
        if lines_up > 0 {
            term.move_cursor_up(lines_up)?;
        }
        // Move cursor to column: 2 spaces left padding + cursor position.
        // Use \r to go to column 0, then move_cursor_right to avoid
        // overwriting rendered content with spaces.
        term.write_str("\r")?;
        let col = 2 + state.cursor_col.min(inner_width);
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
                // Shift+Enter or Alt+Enter → insert newline.
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

/// Render the textbox widget as a list of lines to print.
///
/// Layout: title, horizontal separator, content rows, horizontal separator, hint.
/// No vertical borders — content is left-padded with 2 spaces.
fn render(
    state: &EditorState,
    title: &str,
    placeholder: &str,
    width: usize,
    inner_width: usize,
) -> Vec<String> {
    let mut output = Vec::new();

    // Title line.
    output.push(format!("  {}", style(title).bold()));

    // Top separator — full terminal width.
    output.push(format!("  {}", style("─".repeat(width - 2)).dim()));

    // Content lines.
    let visible_end = (state.scroll_offset + BOX_HEIGHT).min(state.lines.len());
    for row in 0..BOX_HEIGHT {
        let line_idx = state.scroll_offset + row;
        if state.is_empty() && row == 0 {
            // Show placeholder.
            let ph = truncate_str(placeholder, inner_width);
            output.push(format!("  {}", style(ph).dim()));
        } else if line_idx < visible_end {
            let content = truncate_str(&state.lines[line_idx], inner_width);
            output.push(format!("  {content}"));
        } else {
            output.push(String::new());
        }
    }

    // Bottom separator — full terminal width.
    output.push(format!("  {}", style("─".repeat(width - 2)).dim()));

    // Hint line.
    output.push(format!(
        "  {}",
        style("Enter to confirm · Shift+Enter for newline · Esc to cancel").dim()
    ));

    output
}

/// Check if an escape sequence represents Shift+Enter or Alt+Enter.
///
/// Different terminals send different sequences:
/// - Alt+Enter: ESC followed by CR (`\r`) or LF (`\n`)
/// - Shift+Enter (kitty protocol): `\x1b[13;2u` → parsed as `['[','1','3',';','2','u']`
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
#[path = "textbox_tests.rs"]
mod tests;
