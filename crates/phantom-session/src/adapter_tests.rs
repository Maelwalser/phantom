use super::*;

#[test]
fn test_claude_extract_session_id() {
    let adapter = ClaudeAdapter;

    let output = "\
Interactive session ended.

Resume this session with:
claude --resume b6578224-e8f1-4959-8644-20632f24eba8
";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("b6578224-e8f1-4959-8644-20632f24eba8".to_string())
    );
}

#[test]
fn test_claude_extract_no_match() {
    let adapter = ClaudeAdapter;
    assert_eq!(adapter.extract_session_id("no session here"), None);
}

#[test]
fn test_claude_extract_with_ansi_noise() {
    let adapter = ClaudeAdapter;
    // The output buffer may contain ANSI escape codes around the text,
    // but the UUID itself should be clean.
    let output = "claude --resume a1b2c3d4-e5f6-7890-abcd-ef1234567890\r\n";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
    );
}

#[test]
fn test_generic_adapter_no_session() {
    let adapter = GenericAdapter {
        command: "vim".to_string(),
    };
    assert_eq!(
        adapter.extract_session_id("claude --resume aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        None
    );
}

#[test]
fn test_is_claude_command() {
    assert!(is_claude_command("claude"));
    assert!(is_claude_command("/usr/bin/claude"));
    assert!(!is_claude_command("aider"));
    assert!(!is_claude_command("/usr/bin/vim"));
}

#[test]
fn test_session_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let phantom_dir = dir.path();
    let agent_id = AgentId("test-agent".to_string());

    // Create the overlay directory structure.
    std::fs::create_dir_all(phantom_dir.join("overlays").join("test-agent")).unwrap();

    let session = CliSession {
        cli_name: "claude".to_string(),
        session_id: "b6578224-e8f1-4959-8644-20632f24eba8".to_string(),
        last_used: Utc::now(),
    };

    save_session(phantom_dir, &agent_id, &session).unwrap();
    let loaded = load_session(phantom_dir, &agent_id).unwrap();

    assert_eq!(loaded.cli_name, "claude");
    assert_eq!(loaded.session_id, "b6578224-e8f1-4959-8644-20632f24eba8");
}

#[test]
fn test_load_session_missing() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = AgentId("no-such-agent".to_string());
    assert!(load_session(dir.path(), &agent_id).is_none());
}
