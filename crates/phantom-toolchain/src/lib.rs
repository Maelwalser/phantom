//! `phantom-toolchain` — stateless detection of the project's build / test /
//! lint tooling from sentinel files.
//!
//! Phantom's task-context templates talk about "running the test suite" and
//! "running the linter" in abstract verbs (see [`VerificationVerb`]). This
//! crate turns those verbs into concrete commands for the repo in front of us
//! by looking for well-known marker files such as `Cargo.toml`, `package.json`,
//! `go.mod`, `pyproject.toml`, and so on.
//!
//! The detector is **stateless and side-effect-free** — it only reads files.
//! It is intentionally small: no tree-sitter, no git2, no async runtime. The
//! output is a [`Toolchain`] struct with `Option<String>` fields per verb that
//! the CLI can format into the `.phantom-task.md` verification block.
//!
//! # Multi-toolchain monorepos
//!
//! For repos that mix languages (e.g. a Rust backend in `crates/` with a TS
//! frontend in `web/`), [`ToolchainDetector::detect_for_file`] walks up from
//! the file's parent directory until it hits a sentinel, so each file picks
//! the nearest toolchain. Results are memoised by directory for cheap repeat
//! lookups.

mod detect;
mod toolchain;

pub use detect::ToolchainDetector;
pub use toolchain::{DetectedLanguage, Toolchain, VerificationVerb};
