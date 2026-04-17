//! Semantic color, label, and section-formatting helpers for CLI output.

use console::{Style, style};
use phantom_core::changeset::ChangesetStatus;

use crate::commands::status::AgentRunState;

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
    println!("  Run {} {description}", style(command).bold());
}
