use super::*;

#[test]
fn test_git_oid_roundtrip() {
    let hex = "aabbccddee00112233445566778899aabbccddee";
    let original = git2::Oid::from_str(hex).unwrap();

    let phantom_oid = oid_to_git_oid(original);
    let recovered = git_oid_to_oid(&phantom_oid).unwrap();

    assert_eq!(original, recovered);
}
