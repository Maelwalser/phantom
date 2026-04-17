//! Conflict presentation formats.
//!
//! Each format knows how to render a particular kind of conflict into the
//! context file. The dispatcher tries them in a fixed order; the first one
//! that returns `true` wins. `MinimalFormat` always succeeds, so the loop
//! always terminates.
//!
//! Adding a new presentation:
//! 1. Create a file under `formats/` with a struct implementing [`ConflictFormat`].
//! 2. Declare `mod <name>;` below.
//! 3. Insert the format in the desired position in [`dispatch_formats`].

pub(super) mod compact_symbol;
pub(super) mod compact_text;
pub(super) mod diff3_markers;
pub(super) mod minimal;

use super::ResolveConflictContext;

/// Maximum byte size for content passed to `diffy::create_patch`.
/// Myers diff is O(ND); beyond this threshold, diffing can lock the CPU
/// and the LLM cannot process the output anyway.
pub(super) const MAX_DIFF_BYTE_SIZE: usize = 250_000;

/// Shared context passed to each format's `try_write`.
pub(super) struct FormatCtx<'a> {
    pub lang: &'a str,
    pub conflict: &'a ResolveConflictContext,
    pub base_short: &'a str,
    pub parser: &'a phantom_semantic::Parser,
}

/// A strategy for rendering a conflict into markdown.
pub(super) trait ConflictFormat {
    /// Attempt to write this format's output into `out`. Returns `true` if
    /// output was written (format applicable and preconditions met), `false`
    /// if the caller should try the next format in the chain.
    fn try_write(&self, out: &mut String, ctx: &FormatCtx<'_>) -> bool;
}

/// Try each format in order until one produces output.
///
/// `MinimalFormat` always succeeds as the final fallback.
pub(super) fn dispatch(out: &mut String, ctx: &FormatCtx<'_>) {
    let formats: [&dyn ConflictFormat; 4] = [
        &compact_symbol::CompactSymbolFormat,
        &compact_text::CompactTextFormat,
        &diff3_markers::Diff3MarkersFormat,
        &minimal::MinimalFormat,
    ];

    for format in formats {
        if format.try_write(out, ctx) {
            return;
        }
    }
}
