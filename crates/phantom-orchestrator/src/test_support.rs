//! Shared test infrastructure for the orchestrator crate.
//!
//! Provides mock implementations and helper functions used across multiple
//! test modules to avoid duplication.

use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Utc};

use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::error::CoreError;
use phantom_core::event::Event;
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;

// Re-export from phantom-git for backward compatibility with test modules.
pub(crate) use phantom_git::test_support::{advance_trunk, commit_file, init_repo, make_upper};

// ---------------------------------------------------------------------------
// Mock EventStore
// ---------------------------------------------------------------------------

pub(crate) struct MockEventStore {
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

// ---------------------------------------------------------------------------
// Test changeset builder
// ---------------------------------------------------------------------------

/// Create a test changeset with the given ID, base commit, and files.
pub(crate) fn make_changeset(id: &str, base: GitOid, files: Vec<PathBuf>) -> Changeset {
    Changeset {
        id: ChangesetId(id.into()),
        agent_id: AgentId("agent-test".into()),
        task: "test task".into(),
        base_commit: base,
        files_touched: files,
        operations: vec![],
        test_result: None,
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}
