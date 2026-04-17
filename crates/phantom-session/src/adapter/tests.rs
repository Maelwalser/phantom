use chrono::Utc;
use phantom_core::id::AgentId;

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
fn test_claude_extract_uppercase_uuid() {
    let adapter = ClaudeAdapter;
    let output = "claude --resume B6578224-E8F1-4959-8644-20632F24EBA8\n";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("B6578224-E8F1-4959-8644-20632F24EBA8".to_string())
    );
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

// -----------------------------------------------------------------------
// Gemini adapter tests
// -----------------------------------------------------------------------

#[test]
fn test_gemini_extract_session_id() {
    let adapter = GeminiAdapter;
    let output = "\
Session ended.

Resume with:
gemini --resume a1b2c3d4-e5f6-7890-abcd-ef1234567890
";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
    );
}

#[test]
fn test_gemini_extract_short_flag() {
    let adapter = GeminiAdapter;
    let output = "gemini -r a1b2c3d4-e5f6-7890-abcd-ef1234567890\n";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
    );
}

#[test]
fn test_gemini_extract_no_match() {
    let adapter = GeminiAdapter;
    assert_eq!(adapter.extract_session_id("no session here"), None);
}

// -----------------------------------------------------------------------
// OpenCode adapter tests
// -----------------------------------------------------------------------

#[test]
fn test_opencode_extract_session_id() {
    let adapter = OpenCodeAdapter;
    let output = "Session saved.\nopencode --session a1b2c3d4-e5f6-7890-abcd-ef1234567890\n";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
    );
}

#[test]
fn test_opencode_extract_short_flag() {
    let adapter = OpenCodeAdapter;
    let output = "opencode -s ses_abc123xyz\n";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("ses_abc123xyz".to_string())
    );
}

#[test]
fn test_opencode_extract_fallback_uuid() {
    let adapter = OpenCodeAdapter;
    let output = "Session ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890\n";
    assert_eq!(
        adapter.extract_session_id(output),
        Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
    );
}

#[test]
fn test_opencode_extract_no_match() {
    let adapter = OpenCodeAdapter;
    assert_eq!(adapter.extract_session_id("no session here"), None);
}

// -----------------------------------------------------------------------
// Factory tests
// -----------------------------------------------------------------------

#[test]
fn test_adapter_for_claude() {
    assert_eq!(adapter_for("claude").name(), "claude");
    assert_eq!(adapter_for("/usr/bin/claude").name(), "claude");
}

#[test]
fn test_adapter_for_gemini() {
    assert_eq!(adapter_for("gemini").name(), "gemini");
    assert_eq!(adapter_for("/usr/local/bin/gemini").name(), "gemini");
}

#[test]
fn test_adapter_for_opencode() {
    assert_eq!(adapter_for("opencode").name(), "opencode");
    assert_eq!(adapter_for("/usr/local/bin/opencode").name(), "opencode");
}

#[test]
fn test_adapter_for_unknown() {
    assert_eq!(adapter_for("aider").name(), "aider");
    // GenericAdapter stores the full command string as its name.
    assert_eq!(adapter_for("/usr/bin/vim").name(), "/usr/bin/vim");
}

// -----------------------------------------------------------------------
// Session persistence tests
// -----------------------------------------------------------------------

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

#[test]
fn test_session_roundtrip_gemini() {
    let dir = tempfile::tempdir().unwrap();
    let phantom_dir = dir.path();
    let agent_id = AgentId("gemini-agent".to_string());
    std::fs::create_dir_all(phantom_dir.join("overlays").join("gemini-agent")).unwrap();

    let session = CliSession {
        cli_name: "gemini".to_string(),
        session_id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string(),
        last_used: Utc::now(),
    };

    save_session(phantom_dir, &agent_id, &session).unwrap();
    let loaded = load_session(phantom_dir, &agent_id).unwrap();

    assert_eq!(loaded.cli_name, "gemini");
    assert_eq!(loaded.session_id, "a1b2c3d4-e5f6-7890-abcd-ef1234567890");
}

#[test]
fn test_session_roundtrip_opencode() {
    let dir = tempfile::tempdir().unwrap();
    let phantom_dir = dir.path();
    let agent_id = AgentId("opencode-agent".to_string());
    std::fs::create_dir_all(phantom_dir.join("overlays").join("opencode-agent")).unwrap();

    let session = CliSession {
        cli_name: "opencode".to_string(),
        session_id: "ses_abc123xyz".to_string(),
        last_used: Utc::now(),
    };

    save_session(phantom_dir, &agent_id, &session).unwrap();
    let loaded = load_session(phantom_dir, &agent_id).unwrap();

    assert_eq!(loaded.cli_name, "opencode");
    assert_eq!(loaded.session_id, "ses_abc123xyz");
}
