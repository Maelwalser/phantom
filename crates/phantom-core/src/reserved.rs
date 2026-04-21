//! Reserved paths that Phantom must never write to.
//!
//! Corrupting the user's VCS is strictly worse than failing loud. Every write
//! boundary in Phantom (overlay writes, materializer, tree builder, whiteout
//! persistence, trunk checkout) must call [`is_reserved_path`] before touching
//! disk and refuse to proceed on a match.
//!
//! The reserved set is intentionally minimal and conservative:
//! - `.git/` — the user's git repository state
//! - `.phantom/` — Phantom's own state (event DB, overlays, config)
//! - `.whiteouts.json` — overlay whiteout tracking file, belongs only inside
//!   an overlay's `upper/` dir and must never be written to trunk

use std::path::Path;

/// Classification of a reserved path match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservedPathKind {
    /// A component of the path equals `.git`.
    DotGit,
    /// A component of the path equals `.phantom`.
    DotPhantom,
    /// The file name is `.whiteouts.json` at any depth.
    WhiteoutsJson,
}

/// Overlay whiteout tracking file name.
pub const WHITEOUTS_JSON: &str = ".whiteouts.json";

/// Return `Some(kind)` if `path` must never be written by Phantom.
///
/// Matching is by path components so relative, absolute, and nested forms
/// (`.git/HEAD`, `./.git/HEAD`, `foo/.git/HEAD`) are all rejected. The check
/// is case-sensitive on Linux (where phantom-overlay runs); case differences
/// are legitimate on other filesystems so we do not attempt to normalize.
pub fn is_reserved_path(path: &Path) -> Option<ReservedPathKind> {
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        let Some(name) = name.to_str() else { continue };
        match name {
            ".git" => return Some(ReservedPathKind::DotGit),
            ".phantom" => return Some(ReservedPathKind::DotPhantom),
            WHITEOUTS_JSON => return Some(ReservedPathKind::WhiteoutsJson),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn check(path: &str) -> Option<ReservedPathKind> {
        is_reserved_path(&PathBuf::from(path))
    }

    #[test]
    fn dotgit_is_reserved_at_every_depth() {
        assert_eq!(check(".git"), Some(ReservedPathKind::DotGit));
        assert_eq!(check(".git/HEAD"), Some(ReservedPathKind::DotGit));
        assert_eq!(check(".git/config"), Some(ReservedPathKind::DotGit));
        assert_eq!(check(".git/refs/heads/main"), Some(ReservedPathKind::DotGit));
        assert_eq!(
            check("subdir/.git/HEAD"),
            Some(ReservedPathKind::DotGit),
            "nested .git must also be reserved"
        );
        assert_eq!(check("./.git/HEAD"), Some(ReservedPathKind::DotGit));
    }

    #[test]
    fn dotphantom_is_reserved() {
        assert_eq!(check(".phantom"), Some(ReservedPathKind::DotPhantom));
        assert_eq!(
            check(".phantom/events.db"),
            Some(ReservedPathKind::DotPhantom)
        );
        assert_eq!(
            check(".phantom/overlays/agent-a/upper/foo.rs"),
            Some(ReservedPathKind::DotPhantom)
        );
    }

    #[test]
    fn whiteouts_json_is_reserved_at_every_depth() {
        assert_eq!(
            check(".whiteouts.json"),
            Some(ReservedPathKind::WhiteoutsJson)
        );
        assert_eq!(
            check("a/b/.whiteouts.json"),
            Some(ReservedPathKind::WhiteoutsJson)
        );
    }

    #[test]
    fn similar_names_are_not_reserved() {
        assert_eq!(check("git"), None);
        assert_eq!(check("git/HEAD"), None);
        assert_eq!(check(".gitignore"), None);
        assert_eq!(check(".gitattributes"), None);
        assert_eq!(check("my.git.backup"), None);
        assert_eq!(check("phantom"), None);
        assert_eq!(check(".phantomrc"), None);
        assert_eq!(check("phantom.toml"), None);
        assert_eq!(check("whiteouts.json"), None, "leading dot required");
        assert_eq!(check(".whiteouts.jsonl"), None);
    }

    #[test]
    fn ordinary_paths_are_not_reserved() {
        assert_eq!(check("src/main.rs"), None);
        assert_eq!(check("README.md"), None);
        assert_eq!(check("Cargo.toml"), None);
        assert_eq!(check(""), None);
    }
}
