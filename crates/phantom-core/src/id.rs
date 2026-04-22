//! Newtype identifiers used throughout Phantom.
//!
//! Each ID type wraps a simple inner value and provides `Display`, `Serialize`,
//! and `Deserialize` implementations. Keeping IDs as distinct newtypes prevents
//! accidental misuse (e.g. passing an [`AgentId`] where a [`ChangesetId`] is expected).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Unique identifier for a changeset (e.g. `"cs-0042"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangesetId(pub String);

impl fmt::Display for ChangesetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

/// Identifier for an agent (e.g. `"agent-a"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    /// Validate that an agent name is safe for use as a filesystem path component.
    /// Only allows alphanumeric characters, hyphens, and underscores. Max 64 chars.
    pub fn validate(name: &str) -> Result<Self, String> {
        if name.is_empty() {
            return Err("agent name must not be empty".into());
        }
        if name.len() > 64 {
            return Err("agent name must be at most 64 characters".into());
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err(
                "agent name may only contain alphanumeric characters, hyphens, and underscores"
                    .into(),
            );
        }
        if name == "." || name == ".." {
            return Err("agent name must not be '.' or '..'".into());
        }
        Ok(Self(name.to_string()))
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

/// Auto-incrementing event identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub u64);

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Symbol identity in the format `"scope::name::kind"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub String);

impl SymbolId {
    /// Extract the short `name` component from a `"scope::name::kind"` ID.
    ///
    /// Returns the full string as a fallback when the ID has fewer than two
    /// `::` separators (e.g., legacy events or hand-crafted test data).
    #[must_use]
    pub fn name(&self) -> &str {
        // Name is the second-to-last `::`-separated segment. We search from
        // the right because the scope itself may contain `::`.
        let s = &self.0;
        match s.rsplit_once("::") {
            Some((before_kind, _kind)) => match before_kind.rsplit_once("::") {
                Some((_scope, name)) => name,
                None => before_kind,
            },
            None => s,
        }
    }

    /// Extract the `scope` component from a `"scope::name::kind"` ID.
    ///
    /// Returns an empty string when no scope prefix is present.
    #[must_use]
    pub fn scope(&self) -> &str {
        let s = &self.0;
        match s.rsplit_once("::") {
            Some((before_kind, _kind)) => match before_kind.rsplit_once("::") {
                Some((scope, _name)) => scope,
                None => "",
            },
            None => "",
        }
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// BLAKE3 content hash (32 bytes).
///
/// Used to detect whether a symbol's body has changed between two versions
/// of a file without comparing the full content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// Compute a BLAKE3 hash of the given byte slice.
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        Self(*hash.as_bytes())
    }

    /// The all-zeros hash. Used as an "unset" sentinel for fields that may be
    /// missing in older serialized data (e.g. `SymbolEntry::signature_hash`
    /// before the dependency graph was introduced).
    #[must_use]
    pub fn zero() -> Self {
        Self([0u8; 32])
    }

    /// Return `true` if this is the all-zeros sentinel.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 32]
    }

    /// Return the hash as a lowercase hex string (64 chars).
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            use fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// Unique identifier for a plan (e.g. `"plan-20260413-143022"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlanId(pub String);

impl fmt::Display for PlanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

/// Plain 20-byte Git object identifier.
///
/// Stored as raw bytes so that `phantom-core` does **not** depend on `git2`.
/// Conversion to/from `git2::Oid` lives in `phantom-orchestrator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GitOid(pub [u8; 20]);

impl GitOid {
    /// Construct from a raw 20-byte array.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// The all-zeros OID, used as a sentinel for "no commit".
    #[must_use]
    pub fn zero() -> Self {
        Self([0u8; 20])
    }

    /// Return the OID as a lowercase hex string (40 chars).
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0.iter().fold(String::with_capacity(40), |mut s, b| {
            use fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

impl fmt::Display for GitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_determinism() {
        let data = b"hello world";
        let h1 = ContentHash::from_bytes(data);
        let h2 = ContentHash::from_bytes(data);
        assert_eq!(h1, h2);

        let h3 = ContentHash::from_bytes(b"different");
        assert_ne!(h1, h3);
    }

    #[test]
    fn content_hash_hex_length() {
        let h = ContentHash::from_bytes(b"test");
        assert_eq!(h.to_hex().len(), 64);
    }

    #[test]
    fn git_oid_zero() {
        let z = GitOid::zero();
        assert_eq!(z.0, [0u8; 20]);
        assert_eq!(z.to_hex(), "0000000000000000000000000000000000000000");
    }

    #[test]
    fn git_oid_from_bytes_roundtrip() {
        let mut bytes = [0u8; 20];
        bytes[0] = 0xab;
        bytes[19] = 0xcd;
        let oid = GitOid::from_bytes(bytes);
        assert_eq!(oid.0, bytes);
        let hex = oid.to_hex();
        assert!(hex.starts_with("ab"));
        assert!(hex.ends_with("cd"));
    }

    #[test]
    fn display_impls() {
        assert_eq!(ChangesetId("cs-1".into()).to_string(), "cs-1");
        assert_eq!(AgentId("agent-a".into()).to_string(), "agent-a");
        assert_eq!(EventId(42).to_string(), "42");
        assert_eq!(
            SymbolId("mod::foo::Function".into()).to_string(),
            "mod::foo::Function"
        );
    }

    #[test]
    fn agent_id_validate_accepts_valid_names() {
        assert!(AgentId::validate("agent-a").is_ok());
        assert!(AgentId::validate("my_agent").is_ok());
        assert!(AgentId::validate("Agent123").is_ok());
        assert!(AgentId::validate("a").is_ok());
    }

    #[test]
    fn agent_id_validate_rejects_empty() {
        assert!(AgentId::validate("").is_err());
    }

    #[test]
    fn agent_id_validate_rejects_too_long() {
        let long = "a".repeat(65);
        assert!(AgentId::validate(&long).is_err());
        // Exactly 64 should be fine.
        let max = "a".repeat(64);
        assert!(AgentId::validate(&max).is_ok());
    }

    #[test]
    fn agent_id_validate_rejects_path_traversal() {
        assert!(AgentId::validate("../../etc/cron.d").is_err());
        assert!(AgentId::validate("..").is_err());
        assert!(AgentId::validate(".").is_err());
        assert!(AgentId::validate("foo/bar").is_err());
        assert!(AgentId::validate("foo bar").is_err());
    }

    #[test]
    fn agent_id_validate_rejects_special_chars() {
        assert!(AgentId::validate("agent.name").is_err());
        assert!(AgentId::validate("agent@name").is_err());
        assert!(AgentId::validate("agent name").is_err());
    }

    #[test]
    fn serde_changeset_id_roundtrip() {
        let id = ChangesetId("cs-0042".into());
        let json = serde_json::to_string(&id).unwrap();
        let back: ChangesetId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn serde_content_hash_roundtrip() {
        let h = ContentHash::from_bytes(b"phantom");
        let json = serde_json::to_string(&h).unwrap();
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn symbol_id_parts() {
        let id = SymbolId("crate::auth::login::function".into());
        assert_eq!(id.name(), "login");
        assert_eq!(id.scope(), "crate::auth");

        let nested = SymbolId("crate::a::b::c::struct".into());
        assert_eq!(nested.name(), "c");
        assert_eq!(nested.scope(), "crate::a::b");

        let short = SymbolId("name::kind".into());
        assert_eq!(short.name(), "name");
        assert_eq!(short.scope(), "");
    }

    #[test]
    fn serde_git_oid_roundtrip() {
        let oid = GitOid::from_bytes([1; 20]);
        let json = serde_json::to_string(&oid).unwrap();
        let back: GitOid = serde_json::from_str(&json).unwrap();
        assert_eq!(oid, back);
    }
}
