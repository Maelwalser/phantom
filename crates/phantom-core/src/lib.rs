//! `phantom-core` — shared types, traits, and error handling for Phantom.
//!
//! This crate is the foundation of the Phantom workspace. It defines the core
//! domain types ([`Changeset`], [`Event`], [`SymbolEntry`], etc.), newtype
//! identifiers, error types, and the trait interfaces that downstream crates
//! implement.
//!
//! **Design rule:** `phantom-core` has zero dependencies on other Phantom
//! crates. All dependency arrows point inward.

pub mod changeset;
pub mod conflict;
pub mod error;
pub mod event;
pub mod id;
pub mod notification;
pub mod plan;
pub mod symbol;
pub mod traits;

/// Returns `true` if `buf` contains null bytes (in the first 8 000 bytes,
/// matching git's `buffer_is_binary` heuristic) or is not valid UTF-8.
///
/// Such content cannot be safely text-merged by line-based algorithms that
/// require `&str` input.
#[must_use]
pub fn is_binary_or_non_utf8(buf: &[u8]) -> bool {
    let check_len = buf.len().min(8000);
    buf[..check_len].contains(&0) || std::str::from_utf8(buf).is_err()
}

// Re-export the most commonly used types at the crate root for ergonomics.
pub use changeset::{Changeset, ChangesetStatus, SemanticOperation, TestResult};
pub use conflict::{ConflictDetail, ConflictKind, ConflictSpan};
pub use error::CoreError;
pub use event::{Event, EventKind, MergeCheckResult};
pub use id::{AgentId, ChangesetId, ContentHash, EventId, GitOid, PlanId, SymbolId};
pub use notification::{TrunkFileStatus, TrunkNotification};
pub use plan::{Plan, PlanDomain, PlanStatus, RawPlanDomain, RawPlanOutput};
pub use symbol::{SymbolEntry, SymbolKind};
pub use traits::{EventStore, MergeResult, SemanticAnalyzer, SymbolIndex};
