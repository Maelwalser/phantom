//! Category resolution for `ph task`.
//!
//! The user can supply a category in three shapes:
//! 1. A built-in name (`corrective` / `perfective` / `preventive` / `adaptive`).
//! 2. A path to a user-authored `.md` rule file.
//! 3. Inline text captured via the `--custom` textbox.
//!
//! This module owns the [`ResolvedCategory`] type that lives only inside the
//! `task` command. It does not replace [`phantom_core::TaskCategory`]; the
//! plan pipeline still uses the core enum for serialisation.
//!
//! The resolver is deliberately free of I/O for the built-in and path cases
//! — it only reads the filesystem to check that a path exists and has a
//! `.md` extension. Writing the rule file to disk is deferred to
//! [`ResolvedCategory::materialise`], which runs later once the agent id is
//! known.

use std::path::{Path, PathBuf};

use phantom_core::TaskCategory;
use phantom_session::context_file;

use super::TaskArgs;

/// Directory (under `.phantom/`) where inline `--custom` rule bodies live.
pub(super) const CUSTOM_RULES_DIR: &str = "rules/custom";

/// Resolved category variant — what a single `ph task` invocation will use as
/// its system-prompt rule source.
#[derive(Debug, Clone)]
pub(crate) enum ResolvedCategory {
    /// One of the four canonical built-ins. Rule file is written by
    /// [`context_file::ensure_category_rules_dir`] at materialise time.
    Builtin(TaskCategory),
    /// User-supplied markdown file. Absolute path; no copy is made.
    File(PathBuf),
    /// Inline body captured from the `--custom` textbox. Written to
    /// `.phantom/rules/custom/<agent>.md` on materialise.
    Inline { body: String },
}

impl ResolvedCategory {
    /// Return the absolute path that should be passed to
    /// `--append-system-prompt-file`, writing the underlying file when
    /// necessary (for [`ResolvedCategory::Builtin`] and
    /// [`ResolvedCategory::Inline`]).
    pub(crate) fn materialise(&self, phantom_dir: &Path, agent: &str) -> anyhow::Result<PathBuf> {
        match self {
            ResolvedCategory::Builtin(cat) => {
                context_file::ensure_category_rules_dir(phantom_dir)?;
                Ok(context_file::rules_path(phantom_dir, cat))
            }
            ResolvedCategory::File(path) => Ok(path.clone()),
            ResolvedCategory::Inline { body } => {
                let dir = phantom_dir.join(CUSTOM_RULES_DIR);
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{agent}.md"));
                std::fs::write(&path, body)?;
                Ok(path)
            }
        }
    }

    /// Short human label for the "Category" key in the CLI status output.
    pub(crate) fn display_label(&self) -> String {
        match self {
            ResolvedCategory::Builtin(cat) => cat.to_string(),
            ResolvedCategory::File(path) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<unknown>");
                format!("file: {name}")
            }
            ResolvedCategory::Inline { .. } => "custom".to_string(),
        }
    }
}

/// Sentinel error message used to distinguish clean cancellation from real
/// failures. Matched by `task::run` so it can exit with `Ok(())` and print a
/// short "Cancelled" line instead of bubbling up as an error.
pub(super) const CANCELLED: &str = "__phantom_cancelled__";

/// Resolve the category from [`TaskArgs`]. Returns `Ok(None)` when neither
/// flag is present.
pub(super) fn resolve_category(
    args: &TaskArgs,
    repo_root: &Path,
) -> anyhow::Result<Option<ResolvedCategory>> {
    match (args.category.as_ref(), args.custom) {
        (Some(s), false) if s.trim().is_empty() => match prompt_builtin_category()? {
            Some(cat) => Ok(Some(ResolvedCategory::Builtin(cat))),
            None => anyhow::bail!(CANCELLED),
        },
        (Some(value), false) => resolve_value(value.trim(), repo_root).map(Some),
        (None, true) => match prompt_custom_body()? {
            Some(body) if !body.trim().is_empty() => Ok(Some(ResolvedCategory::Inline { body })),
            _ => anyhow::bail!(CANCELLED),
        },
        (None, false) => Ok(None),
        // Clap's `conflicts_with` catches the double-flag case at parse time.
        (Some(_), true) => unreachable!("clap conflicts_with prevents this"),
    }
}

/// Parse a non-empty `--category <value>` argument. Built-in name match wins
/// when present; otherwise the value must point at an existing `.md` file.
pub(super) fn resolve_value(value: &str, repo_root: &Path) -> anyhow::Result<ResolvedCategory> {
    if let Ok(builtin) = value.parse::<TaskCategory>()
        && builtin.is_builtin()
    {
        return Ok(ResolvedCategory::Builtin(builtin));
    }

    let path = PathBuf::from(value);
    let resolved = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or_else(|_| path.clone(), |cwd| cwd.join(&path))
    };

    if !resolved.is_file() {
        anyhow::bail!(
            "category '{value}' is not a built-in and no such file exists (resolved to {})",
            resolved.display()
        );
    }
    if resolved.extension().and_then(|e| e.to_str()) != Some("md") {
        anyhow::bail!(
            "category file '{}' must have a .md extension",
            resolved.display()
        );
    }

    // `repo_root` is not used today but is reserved for future repo-relative
    // anchoring so the signature remains forwards-compatible.
    let _ = repo_root;

    Ok(ResolvedCategory::File(resolved))
}

/// On resume, if the user did not pass a category flag and an inline custom
/// rule file was previously saved for this agent, reuse it so the agent's
/// system prompt persists across sessions.
pub(super) fn implicit_resume_from_custom(
    phantom_dir: &Path,
    agent: &str,
) -> Option<ResolvedCategory> {
    let path = phantom_dir
        .join(CUSTOM_RULES_DIR)
        .join(format!("{agent}.md"));
    path.is_file().then_some(ResolvedCategory::File(path))
}

/// Interactive menu of the four built-in categories. Returns `None` when the
/// user cancels with `Esc` / `Ctrl+C`.
fn prompt_builtin_category() -> anyhow::Result<Option<TaskCategory>> {
    use dialoguer::Select;

    let labels: Vec<String> = TaskCategory::ALL
        .iter()
        .map(|c| format!("{c:<11}  {}", describe(c)))
        .collect();

    let selection = Select::new()
        .with_prompt("Select a task category")
        .items(&labels)
        .default(0)
        .interact_opt()?;

    Ok(selection.map(|i| TaskCategory::ALL[i].clone()))
}

/// One-line description of each built-in category used in the menu.
fn describe(c: &TaskCategory) -> &'static str {
    match c {
        TaskCategory::Corrective => "bug fix — repro test required before code change",
        TaskCategory::Perfective => "refactor / perf / cleanup — tests are read-only",
        TaskCategory::Preventive => "test hardening — source code is read-only",
        TaskCategory::Adaptive => "new feature — must mirror an existing precedent",
        TaskCategory::Custom(_) => "",
    }
}

/// Open the shared multi-line textbox widget. Returns `None` when cancelled.
fn prompt_custom_body() -> anyhow::Result<Option<String>> {
    crate::ui::textbox::multiline_input(
        "Write the custom rule body for this task:",
        "Describe the constraints and requirements for this task...",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn tmp_repo() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn resolve_value_matches_builtin_exact() {
        let repo = tmp_repo();
        let resolved = resolve_value("corrective", repo.path()).unwrap();
        assert!(matches!(
            resolved,
            ResolvedCategory::Builtin(TaskCategory::Corrective)
        ));
    }

    #[test]
    fn resolve_value_is_case_insensitive_for_builtins() {
        let repo = tmp_repo();
        let resolved = resolve_value("ADAPTIVE", repo.path()).unwrap();
        assert!(matches!(
            resolved,
            ResolvedCategory::Builtin(TaskCategory::Adaptive)
        ));
    }

    #[test]
    fn resolve_value_accepts_existing_md_path() {
        let repo = tmp_repo();
        let md_path = repo.path().join("my-rules.md");
        fs::write(&md_path, "# rules\n").unwrap();

        let resolved = resolve_value(md_path.to_str().unwrap(), repo.path()).unwrap();
        match resolved {
            ResolvedCategory::File(p) => assert_eq!(p, md_path),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn resolve_value_rejects_non_md_extension() {
        let repo = tmp_repo();
        let txt_path = repo.path().join("rules.txt");
        fs::write(&txt_path, "x").unwrap();

        let err = resolve_value(txt_path.to_str().unwrap(), repo.path()).unwrap_err();
        assert!(err.to_string().contains(".md"));
    }

    #[test]
    fn resolve_value_rejects_missing_path() {
        let repo = tmp_repo();
        let err = resolve_value("no-such-file.md", repo.path()).unwrap_err();
        assert!(err.to_string().contains("no-such-file.md"));
    }

    #[test]
    fn resolve_value_rejects_unknown_name() {
        let repo = tmp_repo();
        let err = resolve_value("cleanup", repo.path()).unwrap_err();
        assert!(err.to_string().contains("cleanup"));
    }

    #[test]
    fn resolve_value_does_not_accept_custom_prefix_as_builtin() {
        // `custom:foo` parses as TaskCategory::Custom — is_builtin() == false,
        // so the resolver falls through to the path branch and rejects it
        // (no such file). This prevents users from smuggling Custom variants
        // through the CLI path.
        let repo = tmp_repo();
        let err = resolve_value("custom:foo", repo.path()).unwrap_err();
        assert!(err.to_string().contains("custom:foo"));
    }

    #[test]
    fn materialise_builtin_writes_four_files_and_returns_one() {
        let phantom = tmp_repo();
        let phantom_dir = phantom.path();

        let path = ResolvedCategory::Builtin(TaskCategory::Corrective)
            .materialise(phantom_dir, "agent-a")
            .unwrap();

        assert_eq!(path, phantom_dir.join("rules/corrective.md"));
        for cat in &TaskCategory::ALL {
            assert!(phantom_dir.join(format!("rules/{cat}.md")).exists());
        }
    }

    #[test]
    fn materialise_file_returns_path_unchanged() {
        let phantom = tmp_repo();
        let phantom_dir = phantom.path();
        let md_path = phantom_dir.join("external.md");
        fs::write(&md_path, "body\n").unwrap();

        let path = ResolvedCategory::File(md_path.clone())
            .materialise(phantom_dir, "agent-a")
            .unwrap();

        assert_eq!(path, md_path);
        assert!(!phantom_dir.join(CUSTOM_RULES_DIR).exists());
    }

    #[test]
    fn materialise_inline_writes_under_rules_custom_by_agent() {
        let phantom = tmp_repo();
        let phantom_dir = phantom.path();

        let path = ResolvedCategory::Inline {
            body: "# custom body\n".into(),
        }
        .materialise(phantom_dir, "agent-b")
        .unwrap();

        assert_eq!(path, phantom_dir.join(CUSTOM_RULES_DIR).join("agent-b.md"));
        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, "# custom body\n");
    }

    #[test]
    fn materialise_inline_overwrites_per_agent() {
        let phantom = tmp_repo();
        let phantom_dir = phantom.path();

        ResolvedCategory::Inline {
            body: "old\n".into(),
        }
        .materialise(phantom_dir, "a")
        .unwrap();
        let path = ResolvedCategory::Inline {
            body: "new\n".into(),
        }
        .materialise(phantom_dir, "a")
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    }

    #[test]
    fn implicit_resume_finds_existing_custom_file() {
        let phantom = tmp_repo();
        let phantom_dir = phantom.path();
        let dir = phantom_dir.join(CUSTOM_RULES_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.md"), "body").unwrap();

        let resolved = implicit_resume_from_custom(phantom_dir, "a");
        match resolved {
            Some(ResolvedCategory::File(p)) => assert_eq!(p, dir.join("a.md")),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn implicit_resume_returns_none_when_no_file() {
        let phantom = tmp_repo();
        assert!(implicit_resume_from_custom(phantom.path(), "a").is_none());
    }

    #[test]
    fn display_label_matches_variants() {
        assert_eq!(
            ResolvedCategory::Builtin(TaskCategory::Adaptive).display_label(),
            "adaptive"
        );
        assert_eq!(
            ResolvedCategory::File(PathBuf::from("/tmp/my-rules.md")).display_label(),
            "file: my-rules.md"
        );
        assert_eq!(
            ResolvedCategory::Inline { body: "x".into() }.display_label(),
            "custom"
        );
    }
}
