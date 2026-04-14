use super::*;

fn sample_event() -> Event {
    Event {
        id: EventId(1),
        timestamp: Utc::now(),
        changeset_id: ChangesetId("cs-0001".into()),
        agent_id: AgentId("agent-a".into()),
        kind: EventKind::TaskCreated {
            base_commit: GitOid::zero(),
            task: String::new(),
        },
    }
}

#[test]
fn serde_event_roundtrip() {
    let event = sample_event();
    let json = serde_json::to_string(&event).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(event, back);
}

#[test]
fn serde_merge_check_result_roundtrip() {
    let clean = MergeCheckResult::Clean;
    let json = serde_json::to_string(&clean).unwrap();
    let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
    assert_eq!(clean, back);

    let conflicted = MergeCheckResult::Conflicted(vec![ConflictDetail {
        kind: crate::conflict::ConflictKind::BothModifiedSymbol,
        file: PathBuf::from("src/lib.rs"),
        symbol_id: None,
        ours_changeset: ChangesetId("cs-1".into()),
        theirs_changeset: ChangesetId("cs-2".into()),
        description: "test conflict".into(),
        ours_span: None,
        theirs_span: None,
        base_span: None,
    }]);
    let json = serde_json::to_string(&conflicted).unwrap();
    let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
    assert_eq!(conflicted, back);
}

#[test]
fn serde_all_event_kinds() {
    let kinds = vec![
        EventKind::TaskCreated {
            base_commit: GitOid::zero(),
            task: String::new(),
        },
        EventKind::TaskDestroyed,
        EventKind::FileWritten {
            path: PathBuf::from("src/main.rs"),
            content_hash: ContentHash::from_bytes(b"test"),
        },
        EventKind::FileDeleted {
            path: PathBuf::from("old.rs"),
        },
        EventKind::ChangesetSubmitted { operations: vec![] },
        EventKind::ChangesetMergeChecked {
            result: MergeCheckResult::Clean,
        },
        EventKind::ChangesetMaterialized {
            new_commit: GitOid::zero(),
        },
        EventKind::ChangesetConflicted { conflicts: vec![] },
        EventKind::ChangesetDropped {
            reason: "reverted".into(),
        },
        EventKind::TrunkAdvanced {
            old_commit: GitOid::zero(),
            new_commit: GitOid::from_bytes([1; 20]),
        },
        EventKind::AgentNotified {
            agent_id: AgentId("agent-b".into()),
            changed_symbols: vec![SymbolId("mod::foo::Function".into())],
        },
        EventKind::TestsRun(TestResult {
            passed: 5,
            failed: 0,
            skipped: 1,
        }),
        EventKind::LiveRebased {
            old_base: GitOid::zero(),
            new_base: GitOid::from_bytes([2; 20]),
            merged_files: vec![PathBuf::from("src/merged.rs")],
            conflicted_files: vec![PathBuf::from("src/conflict.rs")],
        },
        EventKind::ConflictResolutionStarted {
            conflicts: vec![],
            new_base: Some(GitOid::zero()),
        },
        EventKind::AgentLaunched {
            pid: 12345,
            task: "add rate limiting".into(),
        },
        EventKind::AgentCompleted {
            exit_code: Some(0),
            materialized: true,
        },
        EventKind::PlanCreated {
            plan_id: crate::id::PlanId("plan-001".into()),
            request: "add caching".into(),
            domain_count: 2,
            agent_ids: vec![AgentId("plan-001-cache".into())],
        },
        EventKind::PlanCompleted {
            plan_id: crate::id::PlanId("plan-001".into()),
            succeeded: 2,
            failed: 0,
        },
    ];

    for kind in &kinds {
        let json = serde_json::to_string(kind).unwrap();
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(*kind, back, "round-trip failed for {kind:?}");
    }
}

#[test]
fn unrecognized_variant_deserializes_as_unknown() {
    // Simulate a future EventKind variant that this binary doesn't know about.
    let json = r#""SomeFutureVariant""#;
    let kind: EventKind = serde_json::from_str(json).unwrap();
    assert_eq!(kind, EventKind::Unknown);
}

#[test]
fn unrecognized_variant_with_data_returns_error() {
    // serde(other) only catches unit variants. Data-carrying unknown
    // variants produce a deserialization error at the serde level.
    // Forward compatibility is handled at the store layer:
    // row_to_event catches this error and falls back to Unknown.
    let json = r#"{"NewFeatureEvent":{"field":"value"}}"#;
    let result = serde_json::from_str::<EventKind>(json);
    assert!(result.is_err());
}

#[test]
fn unknown_variant_roundtrips_as_unknown() {
    let kind = EventKind::Unknown;
    let json = serde_json::to_string(&kind).unwrap();
    let back: EventKind = serde_json::from_str(&json).unwrap();
    assert_eq!(back, EventKind::Unknown);
}
