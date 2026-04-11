//! Newtype identifiers used throughout Phantom.
//!
//! Each ID type wraps a simple inner value and provides [`Display`], [`Serialize`],
//! and [`Deserialize`] implementations. Keeping IDs as distinct newtypes prevents
//! accidental misuse (e.g. passing an [`AgentId`] where a [`ChangesetId`] is expected).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Unique identifier for a changeset (e.g. `"cs-0042"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangesetId(pub String);

impl fmt::Display for ChangesetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for an agent (e.g. `"agent-a"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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
        assert_eq!(SymbolId("mod::foo::Function".into()).to_string(), "mod::foo::Function");
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
    fn serde_git_oid_roundtrip() {
        let oid = GitOid::from_bytes([1; 20]);
        let json = serde_json::to_string(&oid).unwrap();
        let back: GitOid = serde_json::from_str(&json).unwrap();
        assert_eq!(oid, back);
    }
}
