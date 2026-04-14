use super::*;

fn agent(name: &str, files: &[&str]) -> (AgentId, Vec<PathBuf>) {
    (
        AgentId(name.into()),
        files.iter().map(PathBuf::from).collect(),
    )
}

#[test]
fn overlap_affects_only_matching_agent() {
    let changed = vec![PathBuf::from("src/db.rs")];
    let agents = vec![
        agent("agent-a", &["src/api.rs"]),
        agent("agent-b", &["src/db.rs", "src/cache.rs"]),
    ];

    let result = RippleChecker::check_ripple(&changed, &agents);

    assert!(!result.contains_key(&AgentId("agent-a".into())));
    assert_eq!(
        result.get(&AgentId("agent-b".into())).unwrap(),
        &vec![PathBuf::from("src/db.rs")]
    );
}

#[test]
fn no_overlap_returns_empty() {
    let changed = vec![PathBuf::from("src/unrelated.rs")];
    let agents = vec![
        agent("agent-a", &["src/api.rs"]),
        agent("agent-b", &["src/db.rs"]),
    ];

    let result = RippleChecker::check_ripple(&changed, &agents);
    assert!(result.is_empty());
}

#[test]
fn multiple_overlapping_files() {
    let changed = vec![PathBuf::from("src/db.rs"), PathBuf::from("src/cache.rs")];
    let agents = vec![agent(
        "agent-a",
        &["src/db.rs", "src/cache.rs", "src/api.rs"],
    )];

    let result = RippleChecker::check_ripple(&changed, &agents);
    let affected = result.get(&AgentId("agent-a".into())).unwrap();
    assert_eq!(affected.len(), 2);
    assert!(affected.contains(&PathBuf::from("src/db.rs")));
    assert!(affected.contains(&PathBuf::from("src/cache.rs")));
}

#[test]
fn same_file_touched_by_multiple_agents() {
    let changed = vec![PathBuf::from("src/shared.rs")];
    let agents = vec![
        agent("agent-a", &["src/shared.rs"]),
        agent("agent-b", &["src/shared.rs", "src/other.rs"]),
    ];

    let result = RippleChecker::check_ripple(&changed, &agents);
    assert_eq!(result.len(), 2);
    assert!(result.contains_key(&AgentId("agent-a".into())));
    assert!(result.contains_key(&AgentId("agent-b".into())));
}

#[test]
fn no_agents_returns_empty() {
    let changed = vec![PathBuf::from("src/main.rs")];
    let result = RippleChecker::check_ripple(&changed, &[]);
    assert!(result.is_empty());
}

#[test]
fn no_changed_files_returns_empty() {
    let agents = vec![agent("agent-a", &["src/api.rs"])];
    let result = RippleChecker::check_ripple(&[], &agents);
    assert!(result.is_empty());
}

#[test]
fn classify_shadowed_when_file_in_upper() {
    let tmp = tempfile::tempdir().unwrap();
    let upper = tmp.path();
    // Create a file in the upper directory to simulate agent modification.
    std::fs::create_dir_all(upper.join("src")).unwrap();
    std::fs::write(upper.join("src/db.rs"), "modified").unwrap();

    let changed = vec![PathBuf::from("src/db.rs"), PathBuf::from("src/api.rs")];
    let classified = classify_trunk_changes(&changed, upper);

    assert_eq!(classified.len(), 2);
    assert_eq!(
        classified[0],
        (PathBuf::from("src/db.rs"), TrunkFileStatus::Shadowed)
    );
    assert_eq!(
        classified[1],
        (PathBuf::from("src/api.rs"), TrunkFileStatus::TrunkVisible)
    );
}

#[test]
fn classify_all_visible_when_upper_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let changed = vec![PathBuf::from("src/main.rs")];
    let classified = classify_trunk_changes(&changed, tmp.path());

    assert_eq!(classified.len(), 1);
    assert_eq!(classified[0].1, TrunkFileStatus::TrunkVisible);
}
