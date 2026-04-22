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
pub mod reserved;
pub mod symbol;
pub mod task_category;
pub mod traits;
pub mod util;

// Re-export the most commonly used types at the crate root for ergonomics.
pub use changeset::{Changeset, ChangesetStatus, SemanticOperation, TestResult};
pub use conflict::{ConflictDetail, ConflictKind, ConflictSpan, MergeCheckResult, MergeResult};
pub use error::CoreError;
pub use event::{Event, EventKind};
pub use id::{AgentId, ChangesetId, ContentHash, EventId, GitOid, PlanId, SymbolId};
pub use notification::{DependencyImpact, ImpactChange, TrunkFileStatus, TrunkNotification};
pub use plan::{Plan, PlanDomain, PlanStatus, RawPlanDomain, RawPlanOutput};
pub use reserved::{ReservedPathKind, WHITEOUTS_JSON, is_reserved_path};
pub use symbol::{
    ReferenceKind, SymbolEntry, SymbolKind, SymbolReference, find_enclosing_symbol,
};
pub use task_category::{ParseTaskCategoryError, TaskCategory};
pub use traits::{DependencyEdge, DependencyGraph, EventStore, SemanticAnalyzer, SymbolIndex};
pub use util::is_binary_or_non_utf8;
