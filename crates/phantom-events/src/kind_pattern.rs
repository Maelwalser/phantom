//! Single source of truth for matching [`phantom_core::event::EventKind`]
//! values in SQL `LIKE` queries against the serialized `kind` column.
//!
//! `EventKind` uses serde's default enum representation, so a variant named
//! `X` with data serializes as `{"X":...}`. Anything that wants to match a
//! specific data-carrying variant via SQL `LIKE` must share the same prefix
//! string as the serde output — otherwise a silent change in the serialized
//! shape would break filters without warning.

/// Build the opening fragment of the serialized JSON for a named data-carrying
/// variant. The caller is expected to append `|| '%'` (or equivalent wildcard)
/// in the SQL clause.
#[inline]
pub(crate) fn like_prefix(kind_name: &str) -> String {
    format!("{{\"{kind_name}\"")
}

/// Full `LIKE` pattern (including trailing `%`) for `EventKind::ChangesetMaterialized`.
#[inline]
pub(crate) fn materialized_prefix() -> String {
    format!("{}%", like_prefix("ChangesetMaterialized"))
}

/// Full `LIKE` pattern for `EventKind::ChangesetMaterializationStarted`.
#[inline]
pub(crate) fn materialization_started_prefix() -> String {
    format!("{}%", like_prefix("ChangesetMaterializationStarted"))
}

/// Full `LIKE` pattern for `EventKind::ChangesetConflicted`.
#[inline]
pub(crate) fn conflicted_prefix() -> String {
    format!("{}%", like_prefix("ChangesetConflicted"))
}

/// Full `LIKE` pattern for `EventKind::ChangesetDropped`.
#[inline]
pub(crate) fn dropped_prefix() -> String {
    format!("{}%", like_prefix("ChangesetDropped"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::event::{EventKind, MaterializationPath};
    use phantom_core::id::GitOid;

    #[test]
    fn like_prefix_matches_serde_output_for_data_variants() {
        let kind = EventKind::ChangesetMaterialized {
            new_commit: GitOid::zero(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.starts_with(&like_prefix("ChangesetMaterialized")),
            "serde output `{json}` diverged from LIKE prefix"
        );
    }

    #[test]
    fn materialized_prefix_has_wildcard_suffix() {
        let p = materialized_prefix();
        assert!(p.ends_with('%'));
        assert!(p.starts_with("{\"ChangesetMaterialized\""));
    }

    #[test]
    fn materialization_started_prefix_matches_serde_output() {
        let kind = EventKind::ChangesetMaterializationStarted {
            parent: GitOid::zero(),
            path: MaterializationPath::Direct,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.starts_with(&like_prefix("ChangesetMaterializationStarted")),
            "serde output `{json}` diverged from LIKE prefix"
        );
    }

    /// Materialized and MaterializationStarted share a prefix up to the
    /// 21st character; the former has a `"` after, the latter has an `S`.
    /// Catch the day someone renames `ChangesetMaterialized` and both
    /// patterns collide into the same wildcard.
    #[test]
    fn fence_pattern_does_not_match_materialized_event() {
        let materialized = serde_json::to_string(&EventKind::ChangesetMaterialized {
            new_commit: GitOid::zero(),
        })
        .unwrap();
        let fence_pat = materialization_started_prefix();
        // LIKE with trailing % is equivalent to starts_with on the prefix
        // portion (minus the %). The materialized JSON must not start with
        // the fence prefix.
        let fence_prefix = fence_pat.trim_end_matches('%');
        assert!(
            !materialized.starts_with(fence_prefix),
            "fence prefix `{fence_prefix}` would match materialized event `{materialized}`"
        );
    }
}
