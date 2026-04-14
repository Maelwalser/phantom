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
#[path = "id_tests.rs"]
mod tests;
