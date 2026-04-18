//! Pre-submit scope audit.
//!
//! When a plan decomposes work into parallel domains, each domain's
//! `PlanDomain` declares `files_to_modify` (the allow-list) and
//! `files_not_to_modify` (the deny-list — files owned by other domains).
//! Those lists are written into the agent's system prompt, but the agent
//! is not technically constrained by FUSE; nothing stops an LLM from
//! editing a file outside its scope.
//!
//! This module scans the set of modified / deleted paths on submit, looks
//! up the agent's plan domain by reading the persisted `plan.json` files
//! under `.phantom/plans/`, and flags any out-of-scope paths. Flagging is
//! purely advisory today (logged + recorded as a tracing event); a
//! future change can escalate to a hard rejection.

use std::path::{Path, PathBuf};

use phantom_core::id::AgentId;
use phantom_core::plan::Plan;

/// The allow/deny lists extracted from a single `PlanDomain`.
#[derive(Debug, Clone)]
pub(super) struct DomainScope {
    pub name: String,
    pub files_to_modify: Vec<PathBuf>,
    pub files_not_to_modify: Vec<String>,
}

/// Locate the domain scope for `agent_id` by scanning every `plan.json`
/// under `<phantom_dir>/plans/`. Returns `None` when the agent is not
/// part of a plan (e.g. a standalone interactive task) — in that case
/// the audit is skipped.
pub(super) fn find_scope(phantom_dir: &Path, agent_id: &AgentId) -> Option<DomainScope> {
    let plans_dir = phantom_dir.join("plans");
    let entries = std::fs::read_dir(&plans_dir).ok()?;
    for entry in entries.flatten() {
        let plan_path = entry.path().join("plan.json");
        if !plan_path.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&plan_path) else {
            continue;
        };
        let Ok(plan) = serde_json::from_slice::<Plan>(&bytes) else {
            continue;
        };
        if let Some(domain) = plan.domains.iter().find(|d| d.agent_id == agent_id.0) {
            return Some(DomainScope {
                name: domain.name.clone(),
                files_to_modify: domain.files_to_modify.clone(),
                files_not_to_modify: domain.files_not_to_modify.clone(),
            });
        }
    }
    None
}

/// A single out-of-scope path flagged by the audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScopeViolation {
    /// The path that was modified or deleted.
    pub path: PathBuf,
    /// Which scope list caught the violation.
    pub reason: ViolationReason,
}

/// Emit a `tracing::warn!` line per violation so the operator can see in
/// the log that an agent stepped outside its declared scope.
///
/// Today this is advisory only — it does not block the submit. A future
/// change can escalate to a hard rejection, at which point the site of
/// escalation is this function.
pub(super) fn log_violations(
    agent_id: &AgentId,
    scope: &DomainScope,
    violations: &[ScopeViolation],
) {
    if violations.is_empty() {
        return;
    }
    for v in violations {
        match &v.reason {
            ViolationReason::DeniedByPattern(pattern) => {
                tracing::warn!(
                    agent = %agent_id,
                    domain = %scope.name,
                    path = %v.path.display(),
                    pattern = %pattern,
                    "scope-audit: path matches files_not_to_modify entry",
                );
            }
            ViolationReason::OutsideAllowList => {
                tracing::warn!(
                    agent = %agent_id,
                    domain = %scope.name,
                    path = %v.path.display(),
                    "scope-audit: path is not covered by files_to_modify",
                );
            }
        }
    }
    tracing::warn!(
        agent = %agent_id,
        domain = %scope.name,
        count = violations.len(),
        "scope-audit: {} out-of-scope path(s) in this submit",
        violations.len(),
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ViolationReason {
    /// Path matched an entry in `files_not_to_modify`.
    DeniedByPattern(String),
    /// Path was not covered by any `files_to_modify` entry.
    OutsideAllowList,
}

/// Audit `modified` + `deleted` paths against `scope`.
///
/// A path is reported when:
/// 1. It matches an entry in `files_not_to_modify` (hard violation), OR
/// 2. It is not covered by any entry in `files_to_modify` (soft violation —
///    the agent either created an unexpected file or wandered outside the
///    listed surface).
///
/// Both cases surface; the caller decides whether to warn or fail.
pub(super) fn audit_paths(
    scope: &DomainScope,
    modified: &[PathBuf],
    deleted: &[PathBuf],
) -> Vec<ScopeViolation> {
    let mut violations = Vec::new();
    for path in modified.iter().chain(deleted.iter()) {
        if let Some(pattern) = matches_any_pattern(path, &scope.files_not_to_modify) {
            violations.push(ScopeViolation {
                path: path.clone(),
                reason: ViolationReason::DeniedByPattern(pattern),
            });
            continue;
        }
        if !scope.files_to_modify.is_empty() && !covered_by_allow_list(path, &scope.files_to_modify)
        {
            violations.push(ScopeViolation {
                path: path.clone(),
                reason: ViolationReason::OutsideAllowList,
            });
        }
    }
    violations
}

/// Return the pattern that matches `path`, if any.
fn matches_any_pattern(path: &Path, patterns: &[String]) -> Option<String> {
    for pattern in patterns {
        if matches_pattern(pattern, path) {
            return Some(pattern.clone());
        }
    }
    None
}

/// True if `path` is covered by at least one entry in the allow-list.
///
/// An allow-list entry is treated as a literal path. A parent directory
/// listed in the allow-list implicitly covers descendants — the planner
/// sometimes writes `src/module/` or `src/module/**` to mean "everything
/// under this module".
fn covered_by_allow_list(path: &Path, allow: &[PathBuf]) -> bool {
    for entry in allow {
        let entry_str = entry.to_string_lossy();
        // Direct match.
        if entry == path {
            return true;
        }
        // Glob-style trailing `**`.
        if entry_str.ends_with("/**") && path.starts_with(entry_str.trim_end_matches("/**")) {
            return true;
        }
        // Treat a trailing slash as "everything under this directory".
        if entry_str.ends_with('/') && path.starts_with(entry_str.trim_end_matches('/')) {
            return true;
        }
    }
    false
}

/// Minimal glob matcher for `files_not_to_modify` patterns.
///
/// Supports:
/// - Literal paths: exact match only.
/// - `dir/**`: matches any path under `dir/`.
/// - `dir/**/*.ext`: matches any `.ext` file under `dir/`.
/// - `*.ext`: matches any file whose basename ends with `.ext`.
///
/// Natural-language `except …` clauses that the planner sometimes appends
/// (e.g. `"crates/foo/src/**/*.rs except lib.rs stub"`) are discarded
/// before matching — the audit errs on the side of NOT matching such
/// ambiguous patterns rather than producing false positives.
fn matches_pattern(pattern: &str, path: &Path) -> bool {
    let pattern = pattern.split(" except ").next().unwrap_or(pattern).trim();
    if pattern.is_empty() {
        return false;
    }
    let path_str = path.to_string_lossy();

    // Exact match.
    if pattern == path_str {
        return true;
    }

    // `dir/**` — any path under dir/.
    if let Some(prefix) = pattern.strip_suffix("/**")
        && !prefix.contains('*')
    {
        return path.starts_with(prefix);
    }

    // `dir/**/*.ext` — descendant with extension.
    if let Some((prefix, suffix)) = split_doublestar(pattern)
        && path_str.starts_with(prefix)
    {
        if suffix == "/*" || suffix.is_empty() {
            return path.starts_with(prefix);
        }
        if let Some(ext) = suffix.strip_prefix("/*.") {
            return path.starts_with(prefix)
                && path.extension().and_then(|e| e.to_str()) == Some(ext);
        }
    }

    // `*.ext` — basename suffix.
    if let Some(ext) = pattern.strip_prefix("*.") {
        return path.extension().and_then(|e| e.to_str()) == Some(ext);
    }

    false
}

/// Split `a/b/**c/d` → Some(("a/b", "c/d")). Returns None if the pattern
/// contains no `/**`.
fn split_doublestar(pattern: &str) -> Option<(&str, &str)> {
    let idx = pattern.find("/**")?;
    let prefix = &pattern[..idx];
    let suffix = &pattern[idx + 3..];
    Some((prefix, suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scope(modify: &[&str], not_modify: &[&str]) -> DomainScope {
        DomainScope {
            name: "test".into(),
            files_to_modify: modify.iter().map(PathBuf::from).collect(),
            files_not_to_modify: not_modify.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn pattern_exact_match() {
        assert!(matches_pattern(
            "src/main.rs",
            &PathBuf::from("src/main.rs")
        ));
        assert!(!matches_pattern(
            "src/main.rs",
            &PathBuf::from("src/lib.rs")
        ));
    }

    #[test]
    fn pattern_double_star_matches_under_prefix() {
        assert!(matches_pattern(
            "crates/foo/**",
            &PathBuf::from("crates/foo/src/lib.rs"),
        ));
        assert!(!matches_pattern(
            "crates/foo/**",
            &PathBuf::from("crates/bar/src/lib.rs"),
        ));
    }

    #[test]
    fn pattern_double_star_with_extension() {
        assert!(matches_pattern(
            "crates/foo/**/*.rs",
            &PathBuf::from("crates/foo/src/lib.rs"),
        ));
        assert!(!matches_pattern(
            "crates/foo/**/*.rs",
            &PathBuf::from("crates/foo/src/lib.toml"),
        ));
    }

    #[test]
    fn pattern_strips_except_clause() {
        // The planner sometimes writes "X except Y"; we treat it as "X".
        assert!(matches_pattern(
            "crates/foo/src/**/*.rs except lib.rs stub",
            &PathBuf::from("crates/foo/src/other.rs"),
        ));
    }

    #[test]
    fn pattern_star_extension() {
        assert!(matches_pattern("*.md", &PathBuf::from("README.md")));
        assert!(matches_pattern("*.md", &PathBuf::from("docs/guide.md")));
        assert!(!matches_pattern("*.md", &PathBuf::from("README.txt")));
    }

    #[test]
    fn audit_flags_denied_pattern() {
        let s = scope(&["src/mine.rs"], &["other/**"]);
        let violations = audit_paths(&s, &[PathBuf::from("other/yours.rs")], &[]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, PathBuf::from("other/yours.rs"));
        match &violations[0].reason {
            ViolationReason::DeniedByPattern(p) => assert_eq!(p, "other/**"),
            ViolationReason::OutsideAllowList => {
                panic!("expected DeniedByPattern, got OutsideAllowList")
            }
        }
    }

    #[test]
    fn audit_flags_outside_allow_list() {
        let s = scope(&["src/mine.rs"], &[]);
        let violations = audit_paths(&s, &[PathBuf::from("src/other.rs")], &[]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].reason, ViolationReason::OutsideAllowList);
    }

    #[test]
    fn audit_passes_when_path_is_in_allow_list() {
        let s = scope(&["src/mine.rs"], &["other/**"]);
        let violations = audit_paths(&s, &[PathBuf::from("src/mine.rs")], &[]);
        assert!(violations.is_empty());
    }

    #[test]
    fn audit_allow_list_directory_covers_children() {
        let s = scope(&["src/**"], &[]);
        let violations = audit_paths(
            &s,
            &[PathBuf::from("src/deep/file.rs"), PathBuf::from("src/x.rs")],
            &[],
        );
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn audit_skips_allow_list_when_empty() {
        // Empty files_to_modify means scope not specified; don't flag every file.
        let s = scope(&[], &["other/**"]);
        let violations = audit_paths(&s, &[PathBuf::from("src/mine.rs")], &[]);
        assert!(violations.is_empty());
    }

    #[test]
    fn audit_checks_deleted_files_too() {
        let s = scope(&["src/mine.rs"], &["other/**"]);
        let violations = audit_paths(&s, &[], &[PathBuf::from("other/gone.rs")]);
        assert_eq!(violations.len(), 1);
        match &violations[0].reason {
            ViolationReason::DeniedByPattern(_) => {}
            ViolationReason::OutsideAllowList => {
                panic!("expected DeniedByPattern, got OutsideAllowList")
            }
        }
    }
}
