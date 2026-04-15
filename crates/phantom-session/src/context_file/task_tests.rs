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
    let updates_pos = content.find("## Trunk Updates").unwrap();
    // Order: most-static to most-dynamic for prompt cache efficiency.
    assert!(commands_pos < info_pos, "Commands should come before Agent Info");
    assert!(info_pos < task_pos, "Agent Info should come before Task");
    assert!(task_pos < updates_pos, "Task should come before Trunk Updates");
}

#[test]
fn context_file_ends_with_updates_section() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("a1".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, None)
        .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    assert!(
        content.contains("## Trunk Updates"),
        "Context file should contain Trunk Updates section"
    );
}

#[test]
fn append_context_update_adds_to_bottom() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("a1".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, Some("task"))
        .unwrap();

    let before = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

    append_context_update(dir.path(), "Agent `b1` submitted changeset `cs-2`.\n")
        .unwrap();

    let after = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

    // Static preamble is preserved byte-for-byte.
    assert!(
        after.starts_with(&before),
        "Appended content must not alter the static preamble"
    );

    // Update is present at the end.
    assert!(
        after.contains("Agent `b1` submitted changeset `cs-2`."),
        "Update should be appended"
    );
}

#[test]
fn append_preserves_preamble_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("x".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-0".to_string());
    let base_commit = phantom_core::id::GitOid([0xAB; 20]);

    write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, None)
        .unwrap();

    let original = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

    // Append two updates.
    append_context_update(dir.path(), "First update\n").unwrap();
    append_context_update(dir.path(), "Second update\n").unwrap();

    let final_content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

    // The original content (including the Trunk Updates header) is an exact prefix.
    assert!(
        final_content.starts_with(&original),
        "Multiple appends must not alter the original content"
    );
    assert!(final_content.contains("First update"));
    assert!(final_content.contains("Second update"));
}

#[test]
fn append_is_noop_when_no_context_file() {
    let dir = tempfile::tempdir().unwrap();
    // No context file written — append should succeed silently.
    append_context_update(dir.path(), "should not crash\n").unwrap();

    // No file should have been created.
    assert!(!dir.path().join(CONTEXT_FILE).exists());
}
