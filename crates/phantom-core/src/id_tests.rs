use super::*;

#[test]
fn content_hash_determinism() {
    let data = b"hello world";
    let h1 = ContentHash::from_bytes(data);
    let h2 = ContentHash::from_bytes(data);
    assert_eq!(h1, h2);

    let h3 = ContentHash::from_bytes(b"different");
    assert_ne!(h1, h3);
}

#[test]
fn content_hash_hex_length() {
    let h = ContentHash::from_bytes(b"test");
    assert_eq!(h.to_hex().len(), 64);
}

#[test]
fn git_oid_zero() {
    let z = GitOid::zero();
    assert_eq!(z.0, [0u8; 20]);
    assert_eq!(z.to_hex(), "0000000000000000000000000000000000000000");
}

#[test]
fn git_oid_from_bytes_roundtrip() {
    let mut bytes = [0u8; 20];
    bytes[0] = 0xab;
    bytes[19] = 0xcd;
    let oid = GitOid::from_bytes(bytes);
    assert_eq!(oid.0, bytes);
    let hex = oid.to_hex();
    assert!(hex.starts_with("ab"));
    assert!(hex.ends_with("cd"));
}

#[test]
fn display_impls() {
    assert_eq!(ChangesetId("cs-1".into()).to_string(), "cs-1");
    assert_eq!(AgentId("agent-a".into()).to_string(), "agent-a");
    assert_eq!(EventId(42).to_string(), "42");
    assert_eq!(
        SymbolId("mod::foo::Function".into()).to_string(),
        "mod::foo::Function"
    );
}

#[test]
fn serde_changeset_id_roundtrip() {
    let id = ChangesetId("cs-0042".into());
    let json = serde_json::to_string(&id).unwrap();
    let back: ChangesetId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn serde_content_hash_roundtrip() {
    let h = ContentHash::from_bytes(b"phantom");
    let json = serde_json::to_string(&h).unwrap();
    let back: ContentHash = serde_json::from_str(&json).unwrap();
    assert_eq!(h, back);
}

#[test]
fn serde_git_oid_roundtrip() {
    let oid = GitOid::from_bytes([1; 20]);
    let json = serde_json::to_string(&oid).unwrap();
    let back: GitOid = serde_json::from_str(&json).unwrap();
    assert_eq!(oid, back);
}
