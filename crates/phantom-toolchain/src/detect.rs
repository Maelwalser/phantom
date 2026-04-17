//! Sentinel-file-driven toolchain detection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::trace;

use crate::toolchain::{DetectedLanguage, Toolchain};

/// Stateless detector with a per-directory memoisation cache.
///
/// The cache is keyed by the canonicalised directory path; repeat lookups
/// against the same directory skip the sentinel scan. The cache is private
/// and purely a performance optimisation — all public methods are safe to
/// call concurrently.
#[derive(Debug, Default)]
pub struct ToolchainDetector {
    cache: Mutex<HashMap<PathBuf, Toolchain>>,
}

impl ToolchainDetector {
    /// Create a fresh detector with an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Detect the toolchain at the repository root. Equivalent to
    /// [`detect_for_file`] on any file at the root.
    ///
    /// [`detect_for_file`]: Self::detect_for_file
    pub fn detect_repo_root(&self, root: &Path) -> Toolchain {
        self.detect_at(root)
    }

    /// Detect the toolchain for a specific file by walking up from the file's
    /// parent directory until a sentinel is found or `repo_root` is reached.
    ///
    /// Useful for multi-toolchain monorepos: a file in `web/src/index.ts`
    /// picks up the nearest `package.json`, while a file in `backend/src/lib.rs`
    /// picks up the nearest `Cargo.toml`.
    pub fn detect_for_file(&self, file: &Path, repo_root: &Path) -> Toolchain {
        let start = file.parent().unwrap_or(file);
        let mut current = start;
        loop {
            let toolchain = self.detect_at(current);
            if !toolchain.is_empty() {
                return toolchain;
            }
            if is_same_directory(current, repo_root) {
                return Toolchain::empty();
            }
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => return Toolchain::empty(),
            }
        }
    }

    fn detect_at(&self, dir: &Path) -> Toolchain {
        let key = dir.to_path_buf();
        if let Ok(cache) = self.cache.lock()
            && let Some(hit) = cache.get(&key)
        {
            return hit.clone();
        }
        let fresh = scan_directory(dir);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, fresh.clone());
        }
        fresh
    }
}

fn is_same_directory(a: &Path, b: &Path) -> bool {
    // Best-effort — canonicalisation may fail in test fixtures, fall back to
    // lexical equality.
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Single-directory sentinel scan. First matching sentinel wins. Returns an
/// empty toolchain when no sentinel is present.
fn scan_directory(dir: &Path) -> Toolchain {
    trace!(dir = %dir.display(), "scanning for toolchain sentinels");

    if dir.join("Cargo.toml").is_file() {
        return rust_toolchain();
    }
    if dir.join("go.mod").is_file() {
        return go_toolchain();
    }
    if dir.join("pom.xml").is_file() {
        return jvm_maven_toolchain();
    }
    if dir.join("build.gradle").is_file() || dir.join("build.gradle.kts").is_file() {
        return jvm_gradle_toolchain(dir);
    }
    if dir.join("pyproject.toml").is_file() {
        return python_pyproject_toolchain(dir);
    }
    if dir.join("setup.py").is_file() || dir.join("requirements.txt").is_file() {
        return python_fallback_toolchain();
    }
    if dir.join("Gemfile").is_file() {
        return ruby_toolchain(dir);
    }
    if dir.join("mix.exs").is_file() {
        return elixir_toolchain();
    }
    if dir.join("package.json").is_file() {
        return node_toolchain(dir);
    }

    Toolchain::empty()
}

fn rust_toolchain() -> Toolchain {
    Toolchain {
        language: Some(DetectedLanguage::Rust),
        test_cmd: Some("cargo test".into()),
        build_cmd: Some("cargo build".into()),
        lint_cmd: Some("cargo clippy --all-targets -- -D warnings".into()),
        typecheck_cmd: None,
        format_check_cmd: Some("cargo fmt --check".into()),
    }
}

fn go_toolchain() -> Toolchain {
    Toolchain {
        language: Some(DetectedLanguage::Go),
        test_cmd: Some("go test ./...".into()),
        build_cmd: Some("go build ./...".into()),
        lint_cmd: Some("go vet ./...".into()),
        typecheck_cmd: None,
        format_check_cmd: Some("gofmt -l .".into()),
    }
}

fn jvm_maven_toolchain() -> Toolchain {
    Toolchain {
        language: Some(DetectedLanguage::Jvm),
        test_cmd: Some("mvn test".into()),
        build_cmd: Some("mvn package -DskipTests".into()),
        lint_cmd: None,
        typecheck_cmd: None,
        format_check_cmd: None,
    }
}

fn jvm_gradle_toolchain(dir: &Path) -> Toolchain {
    let driver = if dir.join("gradlew").is_file() {
        "./gradlew"
    } else {
        "gradle"
    };
    Toolchain {
        language: Some(DetectedLanguage::Jvm),
        test_cmd: Some(format!("{driver} test")),
        build_cmd: Some(format!("{driver} build -x test")),
        lint_cmd: None,
        typecheck_cmd: None,
        format_check_cmd: None,
    }
}

fn python_pyproject_toolchain(dir: &Path) -> Toolchain {
    let pyproject = std::fs::read_to_string(dir.join("pyproject.toml")).unwrap_or_default();
    let has_pytest = pyproject.contains("[tool.pytest") || pyproject.contains("\"pytest\"");
    let has_ruff = pyproject.contains("[tool.ruff") || pyproject.contains("\"ruff\"");
    let has_mypy = pyproject.contains("[tool.mypy") || pyproject.contains("\"mypy\"");

    Toolchain {
        language: Some(DetectedLanguage::Python),
        test_cmd: Some(if has_pytest {
            "pytest".into()
        } else {
            "python -m unittest discover".into()
        }),
        build_cmd: None,
        lint_cmd: has_ruff.then(|| "ruff check .".to_string()),
        typecheck_cmd: has_mypy.then(|| "mypy .".to_string()),
        format_check_cmd: has_ruff.then(|| "ruff format --check .".to_string()),
    }
}

fn python_fallback_toolchain() -> Toolchain {
    Toolchain {
        language: Some(DetectedLanguage::Python),
        test_cmd: Some("python -m unittest discover".into()),
        build_cmd: None,
        lint_cmd: None,
        typecheck_cmd: None,
        format_check_cmd: None,
    }
}

fn ruby_toolchain(dir: &Path) -> Toolchain {
    // Prefer rspec if a spec directory exists or rspec appears in the Gemfile.
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).unwrap_or_default();
    let has_rspec = gemfile.contains("rspec") || dir.join("spec").is_dir();
    Toolchain {
        language: Some(DetectedLanguage::Ruby),
        test_cmd: Some(if has_rspec {
            "bundle exec rspec".into()
        } else {
            "bundle exec rake test".into()
        }),
        build_cmd: None,
        lint_cmd: if gemfile.contains("rubocop") {
            Some("bundle exec rubocop".into())
        } else {
            None
        },
        typecheck_cmd: None,
        format_check_cmd: None,
    }
}

fn elixir_toolchain() -> Toolchain {
    Toolchain {
        language: Some(DetectedLanguage::Elixir),
        test_cmd: Some("mix test".into()),
        build_cmd: Some("mix compile --warnings-as-errors".into()),
        lint_cmd: None,
        typecheck_cmd: None,
        format_check_cmd: Some("mix format --check-formatted".into()),
    }
}

fn node_toolchain(dir: &Path) -> Toolchain {
    let raw = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
    let scripts = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("scripts").cloned())
        .unwrap_or(serde_json::Value::Null);
    let script = |name: &str| -> Option<String> {
        scripts
            .get(name)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|_| format!("npm run {name}"))
    };

    let test_cmd = script("test").or_else(|| Some("npm test".into()));
    let build_cmd = script("build");
    let lint_cmd = script("lint");
    let typecheck_cmd = script("typecheck")
        .or_else(|| script("type-check"))
        .or_else(|| {
            dir.join("tsconfig.json")
                .is_file()
                .then(|| "npx tsc --noEmit".to_string())
        });
    let format_check_cmd = script("format:check").or_else(|| script("format-check"));

    Toolchain {
        language: Some(DetectedLanguage::Node),
        test_cmd,
        build_cmd,
        lint_cmd,
        typecheck_cmd,
        format_check_cmd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn detects_rust_from_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "Cargo.toml", "[package]\nname = \"x\"\n");

        let detector = ToolchainDetector::new();
        let t = detector.detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Rust));
        assert_eq!(t.test_cmd.as_deref(), Some("cargo test"));
        assert!(
            t.lint_cmd
                .as_deref()
                .unwrap_or_default()
                .starts_with("cargo clippy"),
            "expected clippy lint command"
        );
    }

    #[test]
    fn detects_go_from_go_mod() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "go.mod", "module example.com/x\n");

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Go));
        assert_eq!(t.test_cmd.as_deref(), Some("go test ./..."));
        assert_eq!(t.typecheck_cmd, None); // go build subsumes typechecking
    }

    #[test]
    fn detects_python_with_pytest_from_pyproject() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "pyproject.toml",
            "[tool.pytest.ini_options]\n[tool.ruff]\n[tool.mypy]\n",
        );

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Python));
        assert_eq!(t.test_cmd.as_deref(), Some("pytest"));
        assert_eq!(t.lint_cmd.as_deref(), Some("ruff check ."));
        assert_eq!(t.typecheck_cmd.as_deref(), Some("mypy ."));
    }

    #[test]
    fn python_fallback_uses_unittest() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "setup.py", "from setuptools import setup\n");

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Python));
        assert_eq!(t.test_cmd.as_deref(), Some("python -m unittest discover"));
        assert_eq!(t.lint_cmd, None);
    }

    #[test]
    fn node_reads_scripts_from_package_json() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"scripts": {"test": "jest", "lint": "eslint .", "build": "webpack"}}"#,
        );

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Node));
        assert_eq!(t.test_cmd.as_deref(), Some("npm run test"));
        assert_eq!(t.lint_cmd.as_deref(), Some("npm run lint"));
        assert_eq!(t.build_cmd.as_deref(), Some("npm run build"));
    }

    #[test]
    fn node_falls_back_to_npm_test_when_no_scripts() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "package.json", r#"{"name": "x"}"#);

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Node));
        assert_eq!(t.test_cmd.as_deref(), Some("npm test"));
    }

    #[test]
    fn node_picks_up_tsc_from_tsconfig() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "package.json", r#"{}"#);
        write(tmp.path(), "tsconfig.json", r#"{}"#);

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.typecheck_cmd.as_deref(), Some("npx tsc --noEmit"));
    }

    #[test]
    fn detects_jvm_gradle_with_wrapper() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "build.gradle.kts", "");
        write(tmp.path(), "gradlew", "#!/bin/sh\n");

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Jvm));
        assert_eq!(t.test_cmd.as_deref(), Some("./gradlew test"));
    }

    #[test]
    fn detects_jvm_maven() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "pom.xml", "<project/>");

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Jvm));
        assert_eq!(t.test_cmd.as_deref(), Some("mvn test"));
    }

    #[test]
    fn detects_ruby_rspec() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "Gemfile", "gem 'rspec'\ngem 'rubocop'\n");

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Ruby));
        assert_eq!(t.test_cmd.as_deref(), Some("bundle exec rspec"));
        assert_eq!(t.lint_cmd.as_deref(), Some("bundle exec rubocop"));
    }

    #[test]
    fn detects_elixir() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "mix.exs", "defmodule X.MixProject do\nend\n");

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert_eq!(t.language, Some(DetectedLanguage::Elixir));
        assert_eq!(t.test_cmd.as_deref(), Some("mix test"));
    }

    #[test]
    fn empty_toolchain_when_no_sentinel() {
        let tmp = TempDir::new().unwrap();

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        assert!(t.is_empty());
        assert_eq!(t.language, None);
    }

    #[test]
    fn cargo_beats_package_json_when_both_present_at_root() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "Cargo.toml", "[package]\n");
        write(tmp.path(), "package.json", r#"{}"#);

        let t = ToolchainDetector::new().detect_repo_root(tmp.path());

        // First-match-wins ordering lists Cargo.toml before package.json.
        assert_eq!(t.language, Some(DetectedLanguage::Rust));
    }

    #[test]
    fn detect_for_file_walks_up_to_nearest_sentinel() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Multi-toolchain monorepo: root has no sentinel; /backend has
        // Cargo.toml; /web has package.json.
        fs::create_dir_all(root.join("backend/src")).unwrap();
        fs::create_dir_all(root.join("web/src")).unwrap();
        write(&root.join("backend"), "Cargo.toml", "[package]\n");
        write(
            &root.join("web"),
            "package.json",
            r#"{"scripts":{"test":"vitest"}}"#,
        );

        let detector = ToolchainDetector::new();

        let rust_file = root.join("backend/src/lib.rs");
        let ts_file = root.join("web/src/index.ts");

        let rust = detector.detect_for_file(&rust_file, root);
        let ts = detector.detect_for_file(&ts_file, root);

        assert_eq!(rust.language, Some(DetectedLanguage::Rust));
        assert_eq!(ts.language, Some(DetectedLanguage::Node));
        assert_eq!(ts.test_cmd.as_deref(), Some("npm run test"));
    }

    #[test]
    fn detect_for_file_returns_empty_when_no_sentinel_up_to_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        let file = root.join("a/b/c.txt");

        let t = ToolchainDetector::new().detect_for_file(&file, root);

        assert!(t.is_empty());
    }

    #[test]
    fn cache_returns_same_result_on_repeat_lookup() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "Cargo.toml", "[package]\n");

        let detector = ToolchainDetector::new();
        let first = detector.detect_repo_root(tmp.path());
        let second = detector.detect_repo_root(tmp.path());

        assert_eq!(first, second);
    }
}
