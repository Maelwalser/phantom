//! Portable plan file format: a Markdown document that is human-readable
//! and carries an embedded JSON payload so `ph plan --from <file>` can
//! reconstruct the full [`Plan`] without re-invoking the AI planner.
//!
//! Layout:
//! - Top of file: human-readable sections (request, per-domain details).
//! - Bottom: a sentinel HTML comment (`<!-- phantom-plan:json -->`)
//!   immediately followed by a fenced ```` ```` block tagged `json` that
//!   holds the serialized [`Plan`].
//!
//! A 4-backtick fence is used (rather than 3) so the payload can safely
//! contain triple-backticks inside string fields without breaking parsing.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use phantom_core::plan::{Plan, PlanDomain};

const JSON_FENCE_MARKER: &str = "<!-- phantom-plan:json -->";
const FENCE: &str = "````";

/// Render a [`Plan`] as a self-contained Markdown document.
pub fn render_plan_markdown(plan: &Plan) -> String {
    let mut out = String::with_capacity(1024);

    let _ = writeln!(out, "# Phantom Plan");
    let _ = writeln!(out);
    let _ = writeln!(out, "- **ID**: `{}`", plan.id.0);
    let _ = writeln!(
        out,
        "- **Created**: {}",
        plan.created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    let _ = writeln!(out, "- **Domains**: {}", plan.domains.len());
    let _ = writeln!(out);

    let _ = writeln!(out, "## Request");
    let _ = writeln!(out);
    for line in plan.request.lines() {
        let _ = writeln!(out, "> {line}");
    }
    if plan.request.is_empty() {
        let _ = writeln!(out, ">");
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "## Domains");
    let _ = writeln!(out);

    for (idx, domain) in plan.domains.iter().enumerate() {
        render_domain(&mut out, idx + 1, domain);
    }

    let _ = writeln!(out, "---");
    let _ = writeln!(out);
    let _ = writeln!(out, "## Machine-readable payload");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "_Do not edit by hand. `ph plan --from <file>` reads the JSON below._"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "{JSON_FENCE_MARKER}");
    let _ = writeln!(out, "{FENCE}json");
    let payload = serde_json::to_string_pretty(plan).expect("Plan is always serializable to JSON");
    out.push_str(&payload);
    if !payload.ends_with('\n') {
        out.push('\n');
    }
    let _ = writeln!(out, "{FENCE}");

    out
}

fn render_domain(out: &mut String, index: usize, domain: &PlanDomain) {
    let _ = writeln!(out, "### {}. {}", index, domain.name);
    let _ = writeln!(out);
    let _ = writeln!(out, "- **Agent**: `{}`", domain.agent_id);
    let category = domain
        .category
        .as_ref()
        .map_or_else(|| "—".to_string(), ToString::to_string);
    let _ = writeln!(out, "- **Category**: {category}");
    let depends = if domain.depends_on.is_empty() {
        "—".to_string()
    } else {
        domain.depends_on.join(", ")
    };
    let _ = writeln!(out, "- **Depends on**: {depends}");
    let _ = writeln!(out);

    let _ = writeln!(out, "**Description**");
    let _ = writeln!(out);
    for line in domain.description.lines() {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out);

    if !domain.files_to_modify.is_empty() {
        let _ = writeln!(out, "**Files to modify**");
        let _ = writeln!(out);
        for f in &domain.files_to_modify {
            let _ = writeln!(out, "- `{}`", f.display());
        }
        let _ = writeln!(out);
    }

    if !domain.files_not_to_modify.is_empty() {
        let _ = writeln!(out, "**Files NOT to modify**");
        let _ = writeln!(out);
        for f in &domain.files_not_to_modify {
            let _ = writeln!(out, "- `{f}`");
        }
        let _ = writeln!(out);
    }

    if !domain.requirements.is_empty() {
        let _ = writeln!(out, "**Requirements**");
        let _ = writeln!(out);
        for r in &domain.requirements {
            let _ = writeln!(out, "- [ ] {r}");
        }
        let _ = writeln!(out);
    }

    if !domain.verification.is_empty() {
        let _ = writeln!(out, "**Verification**");
        let _ = writeln!(out);
        for v in &domain.verification {
            let _ = writeln!(out, "- `{v}`");
        }
        let _ = writeln!(out);
    }
}

/// Write `plan` to `<repo_root>/phantom-plan-<id>.md` and return the path.
pub fn write_plan_file(repo_root: &Path, plan: &Plan) -> anyhow::Result<PathBuf> {
    let filename = format!("phantom-plan-{}.md", plan.id.0);
    let path = repo_root.join(&filename);
    let contents = render_plan_markdown(plan);
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write plan file {}", path.display()))?;
    Ok(path)
}

/// Parse a plan file produced by [`write_plan_file`] or [`render_plan_markdown`].
///
/// Extracts the JSON payload that follows the `<!-- phantom-plan:json -->`
/// sentinel and deserializes it into a [`Plan`].
pub fn parse_plan_file(path: &Path) -> anyhow::Result<Plan> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read plan file {}", path.display()))?;
    parse_plan_markdown(&contents)
        .with_context(|| format!("failed to parse plan file {}", path.display()))
}

fn parse_plan_markdown(contents: &str) -> anyhow::Result<Plan> {
    let mut lines = contents.lines().peekable();

    // Advance to the sentinel marker.
    let mut found_marker = false;
    for line in lines.by_ref() {
        if line.trim() == JSON_FENCE_MARKER {
            found_marker = true;
            break;
        }
    }
    if !found_marker {
        return Err(anyhow!(
            "not a phantom plan file (missing `{JSON_FENCE_MARKER}` sentinel)"
        ));
    }

    // Expect the next non-empty line to be an opening fence (3+ backticks, optional lang tag).
    let opening = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => {}
            Some(l) => break l.trim_end(),
            None => return Err(anyhow!("unexpected end of file after plan sentinel")),
        }
    };

    let fence_len = opening.chars().take_while(|&c| c == '`').count();
    if fence_len < 3 {
        return Err(anyhow!(
            "expected a fenced code block after plan sentinel, got `{opening}`"
        ));
    }
    let closing_fence: String = "`".repeat(fence_len);

    // Collect everything until the matching closing fence.
    let mut payload = String::new();
    let mut closed = false;
    for line in lines {
        if line.trim_start().starts_with(&closing_fence)
            && line.trim_start()[closing_fence.len()..]
                .chars()
                .all(char::is_whitespace)
        {
            closed = true;
            break;
        }
        payload.push_str(line);
        payload.push('\n');
    }
    if !closed {
        return Err(anyhow!("unterminated JSON fence in plan file"));
    }

    serde_json::from_str::<Plan>(&payload).context("malformed JSON payload in plan file")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use phantom_core::id::PlanId;
    use phantom_core::plan::{Plan, PlanDomain, PlanStatus};
    use phantom_core::task_category::TaskCategory;
    use tempfile::tempdir;

    use super::*;

    fn sample_plan() -> Plan {
        Plan {
            id: PlanId("plan-20260418-120000".into()),
            request: "Add rate limiting and caching".into(),
            created_at: Utc::now(),
            domains: vec![
                PlanDomain {
                    name: "rate-limiting".into(),
                    agent_id: "rate-limiting".into(),
                    description: "Add a token-bucket rate limiter".into(),
                    files_to_modify: vec![PathBuf::from("src/middleware.rs")],
                    files_not_to_modify: vec!["src/cache.rs".into()],
                    requirements: vec!["Token bucket".into(), "Per-IP".into()],
                    verification: vec!["cargo test".into()],
                    depends_on: vec![],
                    category: Some(TaskCategory::Adaptive),
                },
                PlanDomain {
                    name: "cache".into(),
                    agent_id: "cache".into(),
                    description: "LRU cache in front of DB".into(),
                    files_to_modify: vec![PathBuf::from("src/cache.rs")],
                    files_not_to_modify: vec![],
                    requirements: vec!["LRU with TTL".into()],
                    verification: vec!["cargo test".into()],
                    depends_on: vec!["rate-limiting".into()],
                    category: None,
                },
            ],
            status: PlanStatus::Draft,
        }
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let original = sample_plan();
        let md = render_plan_markdown(&original);
        assert!(md.contains(JSON_FENCE_MARKER));
        assert!(md.contains("# Phantom Plan"));

        let parsed = parse_plan_markdown(&md).expect("roundtrip parse");
        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.request, original.request);
        assert_eq!(parsed.domains.len(), original.domains.len());
        for (a, b) in parsed.domains.iter().zip(original.domains.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.agent_id, b.agent_id);
            assert_eq!(a.description, b.description);
            assert_eq!(a.files_to_modify, b.files_to_modify);
            assert_eq!(a.files_not_to_modify, b.files_not_to_modify);
            assert_eq!(a.requirements, b.requirements);
            assert_eq!(a.verification, b.verification);
            assert_eq!(a.depends_on, b.depends_on);
            assert_eq!(a.category, b.category);
        }
        assert_eq!(parsed.status, original.status);
    }

    #[test]
    fn write_plan_file_produces_expected_path() {
        let tmp = tempdir().unwrap();
        let plan = sample_plan();
        let path = write_plan_file(tmp.path(), &plan).unwrap();
        assert_eq!(
            path.file_name().unwrap(),
            "phantom-plan-plan-20260418-120000.md"
        );
        let parsed = parse_plan_file(&path).unwrap();
        assert_eq!(parsed.id, plan.id);
    }

    #[test]
    fn missing_sentinel_returns_descriptive_error() {
        let bogus = "# Some unrelated markdown\n\nNo plan here.\n";
        let err = parse_plan_markdown(bogus).unwrap_err();
        assert!(
            err.to_string().contains("not a phantom plan file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unterminated_fence_returns_error() {
        let broken = format!("hello\n{JSON_FENCE_MARKER}\n{FENCE}json\n{{\"id\":\"x\"\n");
        let err = parse_plan_markdown(&broken).unwrap_err();
        assert!(
            err.to_string().contains("unterminated"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn payload_with_embedded_triple_backticks_survives_roundtrip() {
        let mut plan = sample_plan();
        // User request that contains a triple-backtick — must not break the 4-backtick fence.
        plan.request = "Support ```rust``` snippets in docs".into();
        let md = render_plan_markdown(&plan);
        let parsed = parse_plan_markdown(&md).unwrap();
        assert_eq!(parsed.request, plan.request);
    }

    #[test]
    fn malformed_json_payload_returns_error() {
        let bad =
            format!("hello\n\n{JSON_FENCE_MARKER}\n{FENCE}json\n{{ not valid json }}\n{FENCE}\n");
        let err = parse_plan_markdown(&bad).unwrap_err();
        assert!(
            err.to_string().contains("malformed JSON"),
            "unexpected error: {err}"
        );
    }
}
