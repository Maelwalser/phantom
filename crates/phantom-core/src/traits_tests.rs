use super::*;

#[test]
fn serde_merge_result_roundtrip() {
    let clean = MergeResult::Clean(b"merged output".to_vec());
    let json = serde_json::to_string(&clean).unwrap();
    let back: MergeResult = serde_json::from_str(&json).unwrap();
    assert_eq!(clean, back);

    let conflict = MergeResult::Conflict(vec![]);
    let json = serde_json::to_string(&conflict).unwrap();
    let back: MergeResult = serde_json::from_str(&json).unwrap();
    assert_eq!(conflict, back);
}
