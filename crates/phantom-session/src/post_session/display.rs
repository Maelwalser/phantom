//! Terminal output helpers for the post-session submit flow.
//!
//! Kept separate from [`super::submit`] so the orchestration layer stays
//! testable without stubbing stdout.

use phantom_core::id::{AgentId, ChangesetId};
use phantom_orchestrator::materializer::MaterializeResult;
use phantom_orchestrator::submit_service::SubmitAndMaterializeOutput;

/// Print the file change summary for a successful submit.
pub(super) fn print_submit_summary(out: &SubmitAndMaterializeOutput) {
    println!(
        "  {} additions, {} modifications, {} deletions across {} file(s)",
        out.submit.additions,
        out.submit.modifications,
        out.submit.deletions,
        out.submit.modified_files.len()
    );
    for f in &out.submit.modified_files {
        println!("    {}", f.display());
    }
}

/// Print the success line ("Submitted <cs> -> commit <sha>"), truncated to
/// the terminal width. Also warns about text-fallback merges.
pub(super) fn print_materialize_success(changeset_id: &ChangesetId, result: &MaterializeResult) {
    let MaterializeResult::Success {
        new_commit,
        text_fallback_files,
    } = result
    else {
        return;
    };

    let hex = new_commit.to_hex();
    let short = &hex[..12.min(hex.len())];
    let msg = format!("Submitted {changeset_id} -> commit {short}");
    let width = term_width();
    println!("{}", truncate_line(&msg, width));
    if !text_fallback_files.is_empty() {
        eprintln!(
            "  Warning: {} file(s) merged via line-based fallback (no syntax validation)",
            text_fallback_files.len()
        );
    }
}

/// Print a conflict summary and the follow-up commands the user can run.
pub(super) fn print_conflicts(
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    details: &[phantom_core::ConflictDetail],
) {
    eprintln!("Submission failed with {} conflict(s):", details.len());
    for detail in details {
        eprintln!(
            "  [{:?}] {} -- {}",
            detail.kind,
            detail.file.display(),
            detail.description
        );
    }
    eprintln!();
    eprintln!("The changeset has been submitted but could not be merged.");
    eprintln!(
        "Run `ph resolve {agent_id}` to attempt resolution, or \
         `ph rollback --changeset {changeset_id}` to drop it."
    );
}

/// Return the terminal width in columns, defaulting to 80.
fn term_width() -> usize {
    // SAFETY: `ioctl(TIOCGWINSZ)` reads into a zero-initialised `winsize`.
    // `STDOUT_FILENO` is always a valid file descriptor at process start, and
    // a non-TTY fd fails cleanly rather than aliasing memory.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            ws.ws_col as usize
        } else {
            80
        }
    }
}

/// Truncate `text` so it fits in `max` columns, appending "..." if trimmed.
fn truncate_line(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    if max < 4 {
        return text.chars().take(max).collect();
    }
    let limit = max - 3;
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}
