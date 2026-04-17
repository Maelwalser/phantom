//! Human-readable labels for [`phantom_core::ConflictKind`].
//!
//! Kept exhaustive (no wildcard arm) so the compiler flags new variants added
//! upstream in `phantom-core` before they ship with a blank label.

/// Map a conflict kind to a human-readable label.
pub(super) fn format_conflict_kind(kind: phantom_core::ConflictKind) -> &'static str {
    match kind {
        phantom_core::ConflictKind::BothModifiedSymbol => "BothModifiedSymbol",
        phantom_core::ConflictKind::ModifyDeleteSymbol => "ModifyDeleteSymbol",
        phantom_core::ConflictKind::BothModifiedDependencyVersion => {
            "BothModifiedDependencyVersion"
        }
        phantom_core::ConflictKind::RawTextConflict => "RawTextConflict",
        phantom_core::ConflictKind::BinaryFile => "BinaryFile",
    }
}
