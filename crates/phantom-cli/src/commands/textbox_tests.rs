use super::*;

#[test]
fn insert_char_at_end() {
    let mut state = EditorState::new();
    state.insert_char('a');
    state.insert_char('b');
    assert_eq!(state.lines[0], "ab");
    assert_eq!(state.cursor_col, 2);
}

#[test]
fn insert_char_in_middle() {
    let mut state = EditorState::new();
    state.insert_char('a');
    state.insert_char('c');
    state.cursor_col = 1;
    state.insert_char('b');
    assert_eq!(state.lines[0], "abc");
    assert_eq!(state.cursor_col, 2);
}

#[test]
fn backspace_at_start_joins_lines() {
    let mut state = EditorState::new();
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
    let mut state = EditorState::new();
    state.lines = vec!["abc".into()];
    state.cursor_col = 2;
    state.backspace();
    assert_eq!(state.lines[0], "ac");
    assert_eq!(state.cursor_col, 1);
}

#[test]
fn insert_newline_splits_line() {
    let mut state = EditorState::new();
    state.lines = vec!["helloworld".into()];
    state.cursor_col = 5;
    state.insert_newline();
    assert_eq!(state.lines, vec!["hello", "world"]);
    assert_eq!(state.cursor_row, 1);
    assert_eq!(state.cursor_col, 0);
}

#[test]
fn delete_joins_with_next_line() {
    let mut state = EditorState::new();
    state.lines = vec!["hello".into(), "world".into()];
    state.cursor_col = 5; // at end of first line
    state.delete();
    assert_eq!(state.lines, vec!["helloworld"]);
}

#[test]
fn move_right_wraps_to_next_line() {
    let mut state = EditorState::new();
    state.lines = vec!["ab".into(), "cd".into()];
    state.cursor_col = 2; // at end of first line
    state.move_right();
    assert_eq!(state.cursor_row, 1);
    assert_eq!(state.cursor_col, 0);
}

#[test]
fn move_left_wraps_to_previous_line() {
    let mut state = EditorState::new();
    state.lines = vec!["ab".into(), "cd".into()];
    state.cursor_row = 1;
    state.cursor_col = 0;
    state.move_left();
    assert_eq!(state.cursor_row, 0);
    assert_eq!(state.cursor_col, 2);
}

#[test]
fn scroll_offset_adjusts_when_cursor_below_view() {
    let mut state = EditorState::new();
    state.lines = (0..20).map(|i| format!("line {i}")).collect();
    state.cursor_row = 0;
    state.scroll_offset = 0;
    // Move cursor beyond visible area.
    state.cursor_row = 15;
    state.ensure_cursor_visible();
    assert!(state.scroll_offset > 0);
    assert!(state.cursor_row < state.scroll_offset + BOX_HEIGHT);
}

#[test]
fn to_string_joins_lines() {
    let mut state = EditorState::new();
    state.lines = vec!["hello".into(), "world".into()];
    assert_eq!(state.to_string(), "hello\nworld");
}

#[test]
fn is_empty_when_single_empty_line() {
    let state = EditorState::new();
    assert!(state.is_empty());
}

#[test]
fn is_not_empty_after_typing() {
    let mut state = EditorState::new();
    state.insert_char('a');
    assert!(!state.is_empty());
}

#[test]
fn move_up_clamps_cursor_col() {
    let mut state = EditorState::new();
    state.lines = vec!["hi".into(), "hello world".into()];
    state.cursor_row = 1;
    state.cursor_col = 10;
    state.move_up();
    assert_eq!(state.cursor_row, 0);
    assert_eq!(state.cursor_col, 2); // clamped to "hi".len()
}
