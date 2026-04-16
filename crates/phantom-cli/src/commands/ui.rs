//! Shared CLI styling helpers for consistent, scannable terminal output.

use chrono::{DateTime, Utc};
use console::{Style, style};
use phantom_core::changeset::ChangesetStatus;

use super::status::AgentRunState;

// ── Relative timestamps ──────────────────────────────────────────────

/// Format a timestamp as a human-friendly relative string.
///
/// Accepts an explicit `now` for testability.
pub fn format_relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now.signed_duration_since(ts);
    let secs = elapsed.num_seconds();

    if secs < 0 {
        return "just now".into();
    }
    if secs < 60 {
        return "just now".into();
    }
    if secs < 3600 {
        let mins = elapsed.num_minutes();
        return format!("{mins}m ago");
    }
    if secs < 86400 {
        let hours = elapsed.num_hours();
        return format!("{hours}h ago");
    }

    let days = elapsed.num_days();
    if days == 1 {
        return "yesterday".into();
    }
    if days < 7 {
        return format!("{days}d ago");
    }

    // Older than a week: abbreviated date
    ts.format("%b %d").to_string()
}

/// Format a timestamp relative to now.
pub fn relative_time(ts: DateTime<Utc>) -> String {
    format_relative_time(ts, Utc::now())
}

/// Return a dim-styled relative timestamp for use in listings.
pub fn dim_timestamp(ts: DateTime<Utc>) -> console::StyledObject<String> {
    Style::new().dim().apply_to(relative_time(ts))
}

// ── Semantic color helpers ───────────────────────────────────────────

#[allow(dead_code)]
pub fn style_success(text: &str) -> console::StyledObject<&str> {
    style(text).green()
}

pub fn style_warning(text: &str) -> console::StyledObject<&str> {
    style(text).yellow()
}

pub fn style_error(text: &str) -> console::StyledObject<&str> {
    style(text).red()
}

pub fn style_dim(text: &str) -> console::StyledObject<&str> {
    style(text).dim()
}

pub fn style_bold(text: &str) -> console::StyledObject<&str> {
    style(text).bold()
}

pub fn style_cyan(text: &str) -> console::StyledObject<&str> {
    style(text).cyan()
}

// ── Section formatting ───────────────────────────────────────────────

/// Print a bold section header with a dim rule line underneath.
pub fn section_header(title: &str) {
    let width = console::Term::stdout().size().1.min(80) as usize;
    let rule_len = width.saturating_sub(2);
    println!("  {}", style(title).bold());
    println!("  {}", style("─".repeat(rule_len)).dim());
}

/// Print a key-value pair with dim key, indented.
pub fn key_value(key: &str, value: impl std::fmt::Display) {
    println!(
        "  {}  {value}",
        Style::new().dim().apply_to(format!("{key:<12}"))
    );
}

// ── Changeset status styling ─────────────────────────────────────────

/// Return a colored, human-readable label for a changeset status.
pub fn status_label(status: ChangesetStatus) -> console::StyledObject<&'static str> {
    match status {
        ChangesetStatus::InProgress => style("in progress").dim(),
        ChangesetStatus::Submitted => style("submitted").green(),
        ChangesetStatus::Conflicted => style("conflicted").red(),
        ChangesetStatus::Resolving => style("resolving").cyan(),
        ChangesetStatus::Dropped => style("dropped").dim(),
    }
}

// ── Agent run-state indicators ───────────────────────────────────────

/// Styled indicator symbol for an agent run state.
pub fn run_state_indicator(state: &AgentRunState) -> console::StyledObject<&'static str> {
    match state {
        AgentRunState::Running { .. } => style("●").yellow(),
        AgentRunState::WaitingForDependencies { .. } => style("◌").cyan(),
        AgentRunState::Finished => style("✓").green(),
        AgentRunState::Failed { .. } => style("✗").red(),
        AgentRunState::Idle => style("○").dim(),
    }
}

/// Short text label for a run state, colored appropriately.
pub fn run_state_text(state: &AgentRunState) -> console::StyledObject<&'static str> {
    match state {
        AgentRunState::Running { .. } => style("running").yellow(),
        AgentRunState::WaitingForDependencies { .. } => style("waiting").cyan(),
        AgentRunState::Finished => style("finished").green(),
        AgentRunState::Failed { .. } => style("failed").red(),
        AgentRunState::Idle => style("idle").dim(),
    }
}

// ── Composite message helpers ───────────────────────────────────────

/// Print a styled empty-state message with an optional hint.
///
/// ```text
///   · No events found.
///     Use --since to broaden the search.
/// ```
pub fn empty_state(message: &str, hint: Option<&str>) {
    println!("  {} {}", style("·").dim(), style(message).dim());
    if let Some(hint) = hint {
        println!("    {}", style(hint).dim());
    }
}

/// Print a success message with a green checkmark.
///
/// ```text
///   ✓ Phantom initialized in /home/user/project
/// ```
#[allow(dead_code)]
pub fn success_message(message: impl std::fmt::Display) {
    println!("  {} {message}", style("✓").green());
}

/// Print a warning message to stderr with a yellow warning symbol.
///
/// ```text
///   ⚠ File overlap detected between parallel domains
/// ```
pub fn warning_message(message: impl std::fmt::Display) {
    eprintln!("  {} {message}", style("⚠").yellow());
}

/// Print a dim action hint pointing the user to a follow-up command.
///
/// ```text
///   Run `ph status agent-a` to check progress.
/// ```
pub fn action_hint(command: &str, description: &str) {
    println!(
        "  Run {} {description}",
        style(command).bold()
    );
}

// ── Line truncation ─────────────────────────────────────────────────

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

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
    }

    #[test]
    fn just_now_under_one_minute() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::seconds(30);
        assert_eq!(format_relative_time(t, now), "just now");
    }

    #[test]
    fn minutes_ago() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::minutes(5);
        assert_eq!(format_relative_time(t, now), "5m ago");
    }

    #[test]
    fn hours_ago() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::hours(3);
        assert_eq!(format_relative_time(t, now), "3h ago");
    }

    #[test]
    fn yesterday() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::days(1);
        assert_eq!(format_relative_time(t, now), "yesterday");
    }

    #[test]
    fn days_ago() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now - chrono::Duration::days(4);
        assert_eq!(format_relative_time(t, now), "4d ago");
    }

    #[test]
    fn older_than_a_week_shows_date() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = ts(2026, 3, 20, 10, 0);
        let result = format_relative_time(t, now);
        assert_eq!(result, "Mar 20");
    }

    #[test]
    fn future_timestamp_shows_just_now() {
        let now = ts(2026, 4, 14, 12, 0);
        let t = now + chrono::Duration::hours(1);
        assert_eq!(format_relative_time(t, now), "just now");
    }

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
