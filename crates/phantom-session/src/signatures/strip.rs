//! Body-stripping helpers that reduce function/method source text to just the
//! signature. Separated from the extraction pipeline so the language-specific
//! parsing rules live in one place.

/// Strip the body from a function/method, keeping only the signature. Dispatches
/// on file extension — Python uses indentation-based bodies, everything else is
/// brace-delimited.
pub(super) fn strip_body(source: &str, file_ext: &str) -> String {
    if file_ext == "py" {
        strip_python_body(source)
    } else {
        strip_brace_body(source)
    }
}

/// Strip a brace-delimited body (Rust, TypeScript, Go, Java, C#, ...).
///
/// Finds the first `{` at bracket-depth 0, which starts the function body,
/// and replaces everything from there with `{ ... }`.
pub(super) fn strip_brace_body(source: &str) -> String {
    let mut depth: i32 = 0;
    for (i, ch) in source.char_indices() {
        match ch {
            '(' | '[' | '<' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            '>' if depth > 0 => depth -= 1,
            '{' if depth == 0 => {
                let sig = source[..i].trim_end();
                return format!("{sig} {{ ... }}");
            }
            _ => {}
        }
    }
    // No body found — return as-is (e.g. abstract method declaration).
    source.to_string()
}

/// Strip a Python function body (everything after the signature colon).
pub(super) fn strip_python_body(source: &str) -> String {
    let mut depth: i32 = 0;
    let mut past_params = false;
    for (i, ch) in source.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth = (depth - 1).max(0);
                if depth == 0 {
                    past_params = true;
                }
            }
            ':' if depth == 0 && past_params => {
                return source[..=i].trim_end().to_string();
            }
            _ => {}
        }
    }
    // Fallback: take first line.
    source.lines().next().unwrap_or(source).to_string()
}
