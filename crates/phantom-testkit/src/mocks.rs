//! Mock implementations of phantom-core traits for testing.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use phantom_core::changeset::SemanticOperation;
use phantom_core::conflict::{MergeReport, MergeResult, MergeStrategy};
use phantom_core::error::CoreError;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

type FailPredicate = Box<dyn Fn(&EventKind) -> bool + Send + Sync>;

/// In-memory EventStore for tests.
///
/// Supports fault injection via [`fail_when`](Self::fail_when) so tests can
/// simulate partial-write windows (e.g. crash between materialization and
/// event append).
pub struct MockEventStore {
    events: RwLock<Vec<Event>>,
    fail_on: RwLock<Option<FailPredicate>>,
}

impl MockEventStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
            fail_on: RwLock::new(None),
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.read().unwrap().clone()
    }

    /// Inject a fault: subsequent [`append`](EventStore::append) calls whose
    /// event kind matches `predicate` will return a storage error instead of
    /// being persisted.
    ///
    /// Used by integration tests to simulate a crash in the narrow window
    /// between trunk materialization and the final `ChangesetSubmitted`
    /// event write (see H-ORC2 in the submit pipeline).
    pub fn fail_when<F>(&self, predicate: F)
    where
        F: Fn(&EventKind) -> bool + Send + Sync + 'static,
    {
        *self.fail_on.write().unwrap() = Some(Box::new(predicate));
    }

    /// Clear any fault predicate previously installed by
    /// [`fail_when`](Self::fail_when).
    pub fn clear_fault(&self) {
        *self.fail_on.write().unwrap() = None;
    }
}

impl Default for MockEventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EventStore for MockEventStore {
    async fn append(&self, event: Event) -> Result<EventId, CoreError> {
        if let Some(pred) = self.fail_on.read().unwrap().as_ref()
            && pred(&event.kind)
        {
            return Err(CoreError::Storage("injected fault".into()));
        }
        let mut events = self.events.write().unwrap();
        let id = EventId(events.len() as u64 + 1);
        events.push(Event { id, ..event });
        Ok(id)
    }

    async fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| e.changeset_id == *id)
            .cloned()
            .collect())
    }

    async fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| e.agent_id == *id)
            .cloned()
            .collect())
    }

    async fn query_all(&self) -> Result<Vec<Event>, CoreError> {
        Ok(self.events.read().unwrap().clone())
    }

    async fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| e.timestamp >= since)
            .cloned()
            .collect())
    }

    async fn latest_event_for_changeset(
        &self,
        id: &ChangesetId,
    ) -> Result<Option<EventId>, CoreError> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .rev()
            .find(|e| e.changeset_id == *id)
            .map(|e| e.id))
    }
}

/// SemanticAnalyzer that returns configurable merge reports per file.
pub struct MockAnalyzer {
    merge_results: HashMap<PathBuf, MergeReport>,
}

impl MockAnalyzer {
    pub fn new() -> Self {
        Self {
            merge_results: HashMap::new(),
        }
    }

    /// Configure a full [`MergeReport`] (result + strategy) for `path`.
    pub fn set_merge_report(&mut self, path: PathBuf, report: MergeReport) {
        self.merge_results.insert(path, report);
    }

    /// Convenience: configure a plain [`MergeResult`] and tag it as produced
    /// by full semantic analysis.
    pub fn set_merge_result(&mut self, path: PathBuf, result: MergeResult) {
        self.merge_results
            .insert(path, MergeReport::semantic(result));
    }
}

impl Default for MockAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticAnalyzer for MockAnalyzer {
    fn extract_symbols(
        &self,
        _path: &Path,
        _content: &[u8],
    ) -> Result<Vec<SymbolEntry>, CoreError> {
        Ok(vec![])
    }

    fn diff_symbols(
        &self,
        _base: &[SymbolEntry],
        _current: &[SymbolEntry],
    ) -> Vec<SemanticOperation> {
        vec![]
    }

    fn three_way_merge(
        &self,
        _base: &[u8],
        _ours: &[u8],
        _theirs: &[u8],
        path: &Path,
    ) -> Result<MergeReport, CoreError> {
        match self.merge_results.get(path) {
            Some(report) => Ok(report.clone()),
            None => Ok(MergeReport {
                result: MergeResult::Clean(b"default merged content".to_vec()),
                strategy: MergeStrategy::Semantic,
            }),
        }
    }
}
