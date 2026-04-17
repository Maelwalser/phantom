//! Terminal width and text truncation helpers.

/// Return the current terminal width (columns), defaulting to 80.
pub fn term_width() -> usize {
    console::Term::stdout().size().1 as usize
}

/// Truncate `text` so it fits in `max` display columns.
///
/// If the text is longer than `max`, it is cut and "..." is appended. The
/// result (including the ellipsis) is guaranteed to be at most `max` columns
/// wide. When `max < 4` the string is simply hard-truncated.
pub fn truncate_line(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    if max < 4 {
        // Not enough room for even "x..."
        return text.chars().take(max).collect();
    }
    let limit = max - 3; // room for "..."
    // Respect char boundaries.
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_line_short_text_unchanged() {
        assert_eq!(truncate_line("hello", 80), "hello");
    }

    #[test]
    fn truncate_line_exact_fit() {
        let text = "a".repeat(40);
        assert_eq!(truncate_line(&text, 40), text);
    }

    #[test]
    fn truncate_line_adds_ellipsis() {
        let text = "a".repeat(50);
        let result = truncate_line(&text, 40);
        assert_eq!(result.len(), 40);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_line_tiny_width() {
        assert_eq!(truncate_line("hello world", 3), "hel");
    }
}
