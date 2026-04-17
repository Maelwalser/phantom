//! [`Toolchain`], [`DetectedLanguage`], and [`VerificationVerb`] types.

use serde::{Deserialize, Serialize};

/// Abstract verbs that show up in category rule bodies and task templates.
/// They are mapped to concrete commands via [`Toolchain::command_for`] once a
/// toolchain has been detected for the repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationVerb {
    /// Run the project's test suite.
    RunTests,
    /// Run the project's linter (clippy, eslint, ruff, golangci-lint, ...).
    RunLinter,
    /// Run the project's static type checker (tsc, mypy, ...). Many Rust
    /// projects have no separate typecheck because `cargo check` subsumes it —
    /// in that case the detector leaves this field as `None`.
    RunTypecheck,
    /// Run the project's build (`cargo build`, `npm run build`, ...).
    VerifyBuild,
    /// Check formatting without applying it (`cargo fmt --check`,
    /// `gofmt -l .`, ...).
    CheckFormat,
}

impl VerificationVerb {
    /// All verbs in canonical display order.
    pub const ALL: [VerificationVerb; 5] = [
        VerificationVerb::RunTests,
        VerificationVerb::VerifyBuild,
        VerificationVerb::RunLinter,
        VerificationVerb::RunTypecheck,
        VerificationVerb::CheckFormat,
    ];

    /// Human-readable label used in the verification block header, e.g.
    /// "Run the test suite".
    #[must_use]
    pub fn human_label(self) -> &'static str {
        match self {
            VerificationVerb::RunTests => "Run the test suite",
            VerificationVerb::VerifyBuild => "Verify the build succeeds",
            VerificationVerb::RunLinter => "Run the linter",
            VerificationVerb::RunTypecheck => "Run the type checker",
            VerificationVerb::CheckFormat => "Check formatting",
        }
    }
}

/// Primary build toolchain detected for a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedLanguage {
    Rust,
    Node,
    Python,
    Go,
    Jvm,
    Ruby,
    Elixir,
}

impl DetectedLanguage {
    /// Lowercase human-readable label for display.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DetectedLanguage::Rust => "rust",
            DetectedLanguage::Node => "node",
            DetectedLanguage::Python => "python",
            DetectedLanguage::Go => "go",
            DetectedLanguage::Jvm => "jvm",
            DetectedLanguage::Ruby => "ruby",
            DetectedLanguage::Elixir => "elixir",
        }
    }
}

/// Concrete commands resolved for a repo (or subtree within a repo).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    /// Which language / package manager was detected, if any.
    pub language: Option<DetectedLanguage>,
    /// Command to run the test suite.
    pub test_cmd: Option<String>,
    /// Command to verify the build succeeds.
    pub build_cmd: Option<String>,
    /// Command to run the linter.
    pub lint_cmd: Option<String>,
    /// Command to run a separate type checker. `None` when the build itself
    /// subsumes typechecking (Rust, Go).
    pub typecheck_cmd: Option<String>,
    /// Command to check formatting without modifying files.
    pub format_check_cmd: Option<String>,
}

impl Toolchain {
    /// Empty toolchain — no language detected and no commands populated.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Return the concrete command string for a [`VerificationVerb`], or `None`
    /// when the toolchain has no mapping for it.
    #[must_use]
    pub fn command_for(&self, verb: VerificationVerb) -> Option<&str> {
        match verb {
            VerificationVerb::RunTests => self.test_cmd.as_deref(),
            VerificationVerb::VerifyBuild => self.build_cmd.as_deref(),
            VerificationVerb::RunLinter => self.lint_cmd.as_deref(),
            VerificationVerb::RunTypecheck => self.typecheck_cmd.as_deref(),
            VerificationVerb::CheckFormat => self.format_check_cmd.as_deref(),
        }
    }

    /// True when no commands are populated. Callers should skip rendering the
    /// verification block when this is true.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.language.is_none()
            && self.test_cmd.is_none()
            && self.build_cmd.is_none()
            && self.lint_cmd.is_none()
            && self.typecheck_cmd.is_none()
            && self.format_check_cmd.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toolchain_is_empty() {
        let t = Toolchain::empty();
        assert!(t.is_empty());
        for verb in VerificationVerb::ALL {
            assert!(t.command_for(verb).is_none());
        }
    }

    #[test]
    fn command_for_roundtrip() {
        let t = Toolchain {
            language: Some(DetectedLanguage::Rust),
            test_cmd: Some("cargo test".into()),
            build_cmd: Some("cargo build".into()),
            lint_cmd: Some("cargo clippy --all-targets".into()),
            typecheck_cmd: None,
            format_check_cmd: Some("cargo fmt --check".into()),
        };
        assert_eq!(
            t.command_for(VerificationVerb::RunTests),
            Some("cargo test")
        );
        assert_eq!(
            t.command_for(VerificationVerb::VerifyBuild),
            Some("cargo build")
        );
        assert_eq!(t.command_for(VerificationVerb::RunTypecheck), None);
        assert!(!t.is_empty());
    }

    #[test]
    fn serde_roundtrip_preserves_commands() {
        let t = Toolchain {
            language: Some(DetectedLanguage::Node),
            test_cmd: Some("npm test".into()),
            build_cmd: None,
            lint_cmd: Some("npm run lint".into()),
            typecheck_cmd: Some("tsc --noEmit".into()),
            format_check_cmd: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Toolchain = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
