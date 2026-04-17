//! AI planner invocation: builds the prompt, runs the CLI adapter in headless
//! JSON mode, and parses the result into a [`RawPlanOutput`].

use std::path::Path;

use anyhow::Context;
use phantom_core::plan::RawPlanOutput;

/// Run the AI planner to decompose the request into domains.
pub(super) fn run_planner(
    repo_root: &Path,
    phantom_dir: &Path,
    description: &str,
) -> anyhow::Result<RawPlanOutput> {
    let prompt = build_planning_prompt(description);

    let cli_command = crate::context::default_cli(phantom_dir);
    let adapter = phantom_session::adapter::adapter_for(&cli_command);
    let mut cmd = adapter
        .build_headless_command(repo_root, &prompt, &[], None)
        .context("planner CLI does not support headless mode")?;
    cmd.args(["--output-format", "json"]);
    cmd.stdin(std::process::Stdio::null());

    let output = cmd.output().with_context(|| {
        format!("failed to run planner — is '{cli_command}' installed and on PATH?")
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("planner exited with {}: {stderr}", output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Claude with --output-format json wraps the result in a JSON object
    // with a "result" field containing the text. Try parsing that first.
    if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(&stdout)
        && let Some(result_text) = wrapper.get("result").and_then(|v| v.as_str())
        && let Some(parsed) = try_extract_plan_json(result_text)
    {
        return Ok(parsed);
    }

    // Fallback: try extracting JSON directly from stdout.
    if let Some(parsed) = try_extract_plan_json(&stdout) {
        return Ok(parsed);
    }

    anyhow::bail!("failed to parse planner output as plan JSON. Raw output:\n{stdout}")
}

/// Try to extract a `RawPlanOutput` from text that may contain markdown fences
/// or other wrapping around the JSON.
fn try_extract_plan_json(text: &str) -> Option<RawPlanOutput> {
    // Direct parse.
    if let Ok(plan) = serde_json::from_str::<RawPlanOutput>(text) {
        return Some(plan);
    }

    // Extract from markdown code fence.
    let json_str = extract_json_object(text)?;
    serde_json::from_str::<RawPlanOutput>(json_str).ok()
}

/// Extract the outermost JSON object from text by finding the first `{` and last `}`.
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

/// Build the prompt sent to Claude for plan decomposition.
fn build_planning_prompt(description: &str) -> String {
    format!(
        r#"Analyze this codebase and create an implementation plan for the following request:

"{description}"

Decompose the work into independent domains that can be executed in parallel by separate AI agents. Each domain should be a self-contained unit of work that modifies a distinct set of files.

Output ONLY a JSON object with this structure (no markdown fences, no explanation):
{{
  "domains": [
    {{
      "name": "kebab-case-name",
      "description": "What this domain implements",
      "files_to_modify": ["path/to/file1.rs"],
      "files_not_to_modify": ["paths/owned/by/other/domains"],
      "requirements": ["Requirement 1", "Requirement 2"],
      "verification": ["cargo test", "cargo clippy"],
      "depends_on": []
    }}
  ]
}}

Rules:
- Each domain gets its own agent with its own filesystem overlay
- CONFLICT PREVENTION: No two parallel domains (same wave) may list the same file in files_to_modify. If two domains need the same file, one MUST depends_on the other
- Shared config/build files (package.json, tsconfig.json, Cargo.toml, pyproject.toml, go.mod, Makefile, etc.) are the #1 source of merge conflicts. If multiple domains need these, create a "scaffold" or "setup" domain (wave 0) that owns all shared config, and have other domains depends_on it
- For greenfield projects (empty or near-empty repo), ALWAYS create a scaffold domain for project setup and config files as wave 0
- files_not_to_modify MUST list every file owned by another domain to prevent accidental edits
- Use depends_on freely when file sets overlap — correctness matters more than maximum parallelism
- Keep domains focused: 1-5 files each
- Include verification commands appropriate for this project's toolchain
- Names must be unique kebab-case identifiers
- Every domain MUST have at least one requirement and one verification command"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_object_direct() {
        let text = r#"{"domains": [{"name": "test", "description": "d", "requirements": [], "verification": []}]}"#;
        let result = try_extract_plan_json(text);
        assert!(result.is_some());
        assert_eq!(result.unwrap().domains[0].name, "test");
    }

    #[test]
    fn extract_json_from_markdown_fence() {
        let text = "Here's the plan:\n```json\n{\"domains\": [{\"name\": \"cache\", \"description\": \"add cache\", \"requirements\": [\"r1\"], \"verification\": [\"v1\"]}]}\n```\n";
        let result = try_extract_plan_json(text);
        assert!(result.is_some());
        assert_eq!(result.unwrap().domains[0].name, "cache");
    }

    #[test]
    fn extract_json_with_surrounding_text() {
        let text = "I'll create this plan: {\"domains\": [{\"name\": \"api\", \"description\": \"d\", \"requirements\": [], \"verification\": []}]} That should work.";
        let result = try_extract_plan_json(text);
        assert!(result.is_some());
    }
}
