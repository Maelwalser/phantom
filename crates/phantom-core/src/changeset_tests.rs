use super::*;
use crate::id::SymbolId;
use crate::symbol::SymbolKind;

fn sample_changeset() -> Changeset {
    Changeset {
        id: ChangesetId("cs-0001".into()),
        agent_id: AgentId("agent-a".into()),
        task: "add rate limiting".into(),
        base_commit: GitOid::zero(),
        files_touched: vec![PathBuf::from("src/api.rs")],
        operations: vec![SemanticOperation::AddSymbol {
            file: PathBuf::from("src/api.rs"),
            symbol: SymbolEntry {
                id: SymbolId("crate::api::rate_limit::Function".into()),
                kind: SymbolKind::Function,
                name: "rate_limit".into(),
                scope: "crate::api".into(),
                file: PathBuf::from("src/api.rs"),
                byte_range: 0..50,
                content_hash: ContentHash::from_bytes(b"fn rate_limit() {}"),
            },
        }],
        test_result: Some(TestResult {
            passed: 10,
            failed: 0,
            skipped: 1,
        }),
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}

#[test]
fn serde_changeset_status_roundtrip() {
    for status in [
        ChangesetStatus::InProgress,
        ChangesetStatus::Submitted,
        ChangesetStatus::Conflicted,
        ChangesetStatus::Resolving,
        ChangesetStatus::Dropped,
    ] {
        let json = serde_json::to_string(&status).unwrap();
        let back: ChangesetStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
    }
}

#[test]
fn serde_semantic_operation_roundtrip() {
    let ops = vec![
        SemanticOperation::AddFile {
            path: PathBuf::from("new.rs"),
        },
        SemanticOperation::DeleteFile {
            path: PathBuf::from("old.rs"),
        },
        SemanticOperation::RawDiff {
            path: PathBuf::from("config.toml"),
            patch: "+foo = true".into(),
        },
    ];
    for op in &ops {
        let json = serde_json::to_string(op).unwrap();
        let back: SemanticOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(*op, back);
    }
}

#[test]
fn serde_changeset_roundtrip() {
    let cs = sample_changeset();
    let json = serde_json::to_string(&cs).unwrap();
    let back: Changeset = serde_json::from_str(&json).unwrap();
    assert_eq!(cs, back);
}

#[test]
fn serde_test_result_roundtrip() {
    let tr = TestResult {
        passed: 5,
        failed: 2,
        skipped: 0,
    };
    let json = serde_json::to_string(&tr).unwrap();
    let back: TestResult = serde_json::from_str(&json).unwrap();
    assert_eq!(tr, back);
}
