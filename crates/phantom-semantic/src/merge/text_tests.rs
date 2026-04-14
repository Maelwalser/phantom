use super::*;

#[test]
fn binary_file_with_null_bytes_returns_conflict() {
    let base = b"line1\nline2\n";
    let ours = b"line1\x00binary\nline2\n";
    let theirs = b"line1\nline2\nline3\n";

    let result = text_merge(base, ours, theirs, Path::new("data.bin")).unwrap();

    match result {
        MergeResult::Conflict(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
        }
        MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
    }
}

#[test]
fn non_utf8_bytes_returns_conflict() {
    let base = b"valid utf8\n";
    let ours = b"\xff\xfe invalid utf8\n";
    let theirs = b"also valid\n";

    let result = text_merge(base, ours, theirs, Path::new("encoded.txt")).unwrap();

    match result {
        MergeResult::Conflict(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
        }
        MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
    }
}

#[test]
fn valid_utf8_text_merges_normally() {
    let base = b"line1\nline2\nline3\n";
    let ours = b"line1\nmodified\nline3\n";
    let theirs = b"line1\nline2\nline3\nline4\n";

    let result = text_merge(base, ours, theirs, Path::new("notes.txt")).unwrap();

    match result {
        MergeResult::Clean(merged) => {
            let text = std::str::from_utf8(&merged).unwrap();
            assert!(text.contains("modified"));
            assert!(text.contains("line4"));
        }
        MergeResult::Conflict(_) => panic!("expected clean merge"),
    }
}
