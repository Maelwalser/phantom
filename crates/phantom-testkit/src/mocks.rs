//! Mock implementations of phantom-core traits for testing.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use phantom_core::changeset::SemanticOperation;
use phantom_core::conflict::MergeResult;
use phantom_core::error::CoreError;
use phantom_core::event::Event;
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

/// In-memory EventStore for tests.
pub struct MockEventStore {
    events: RwLock<Vec<Event>>,
}

impl MockEventStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.read().unwrap().clone()
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
        let mut events = self.events.write().unwrap();
        let id = EventId(events.len() as u64 + 1);
        events.push(Event { id, ..event });
        Ok(id)
    }

    async fn query_by_changeset(&self, _id: &ChangesetId) -> Result<Vec<Event>, CoreError> {
        Ok(vec![])
    }

    async fn query_by_agent(&self, _id: &AgentId) -> Result<Vec<Event>, CoreError> {
        Ok(vec![])
    }

    async fn query_all(&self) -> Result<Vec<Event>, CoreError> {
        Ok(self.events.read().unwrap().clone())
    }

    async fn query_since(&self, _since: DateTime<Utc>) -> Result<Vec<Event>, CoreError> {
        Ok(vec![])
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

/// SemanticAnalyzer that returns configurable merge results per file.
pub struct MockAnalyzer {
    merge_results: HashMap<PathBuf, MergeResult>,
}

impl MockAnalyzer {
    pub fn new() -> Self {
        Self {
            merge_results: HashMap::new(),
        }
    }

    pub fn set_merge_result(&mut self, path: PathBuf, result: MergeResult) {
        self.merge_results.insert(path, result);
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
    ) -> Result<MergeResult, CoreError> {
        match self.merge_results.get(path) {
            Some(result) => Ok(result.clone()),
            None => Ok(MergeResult::Clean(b"default merged content".to_vec())),
        }
    }
}
