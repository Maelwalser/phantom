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
pub mod symbol;
pub mod traits;

// Re-export the most commonly used types at the crate root for ergonomics.
pub use changeset::{Changeset, ChangesetStatus, SemanticOperation, TestResult};
pub use conflict::{ConflictDetail, ConflictKind};
pub use error::CoreError;
pub use event::{Event, EventKind, MergeCheckResult};
pub use id::{AgentId, ChangesetId, ContentHash, EventId, GitOid, SymbolId};
pub use symbol::{SymbolEntry, SymbolKind};
pub use notification::{TrunkFileStatus, TrunkNotification};
pub use traits::{EventStore, MergeResult, SemanticAnalyzer, SymbolIndex};
