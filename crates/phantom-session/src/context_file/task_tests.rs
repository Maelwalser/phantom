use super::*;

#[test]
fn context_file_has_dynamic_sections_last() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("a1".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, Some("do stuff"))
        .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    let commands_pos = content.find("## Commands").unwrap();
    let info_pos = content.find("## Agent Info").unwrap();
    let task_pos = content.find("## Task").unwrap();
    // Order: most-static to most-dynamic for prompt cache efficiency.
    assert!(commands_pos < info_pos, "Commands should come before Agent Info");
    assert!(info_pos < task_pos, "Agent Info should come before Task");
}
