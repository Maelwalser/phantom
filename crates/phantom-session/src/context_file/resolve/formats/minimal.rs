//! Minimal fallback format — last resort when every other format declined.
//!
//! Picks the best available content (prefer THEIRS, then OURS, then BASE) and
//! renders it as a single labeled, truncated code block. Always returns
//! `true`, so the dispatcher loop always terminates.

use std::fmt::Write;

use super::super::truncate::{compute_truncation_center, write_truncated};
use super::{ConflictFormat, FormatCtx};

pub(super) struct MinimalFormat;

impl ConflictFormat for MinimalFormat {
    fn try_write(&self, out: &mut String, ctx: &FormatCtx<'_>) -> bool {
        let trunc_center = compute_truncation_center(
            ctx.conflict.base_content.as_deref(),
            ctx.conflict.ours_content.as_deref(),
            ctx.conflict.theirs_content.as_deref(),
        );
        write_minimal_fallback(
            out,
            ctx.lang,
            ctx.conflict.base_content.as_deref(),
            ctx.conflict.ours_content.as_deref(),
            ctx.conflict.theirs_content.as_deref(),
            ctx.base_short,
            trunc_center,
        );
        true
    }
}

/// Last-resort fallback when conflict marker generation is not possible
/// (missing content or oversized files). Emits whatever content is available
/// as a single labeled, truncated code block.
pub(in crate::context_file::resolve) fn write_minimal_fallback(
    out: &mut String,
    lang: &str,
    base: Option<&str>,
    ours: Option<&str>,
    theirs: Option<&str>,
    base_short: &str,
    trunc_center: usize,
) {
    // Pick the best available content: prefer theirs (agent's working copy),
    // then ours (trunk), then base.
    let (label, text) = if let Some(t) = theirs {
        ("THEIRS (agent's working directory)".to_string(), t)
    } else if let Some(o) = ours {
        ("OURS (current trunk)".to_string(), o)
    } else if let Some(b) = base {
        (format!("BASE (common ancestor at {base_short})"), b)
    } else {
        writeln!(out, "*(no file content available for any version)*").unwrap();
        writeln!(out).unwrap();
        return;
    };

    writeln!(out, "#### {label}").unwrap();
    writeln!(out, "```{lang}").unwrap();
    write_truncated(out, text, trunc_center);
    out.push('\n');
    writeln!(out, "```").unwrap();
    writeln!(out).unwrap();
}
