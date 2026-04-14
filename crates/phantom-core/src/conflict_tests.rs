use super::*;

fn sample_conflict() -> ConflictDetail {
    ConflictDetail {
        kind: ConflictKind::BothModifiedSymbol,
        file: PathBuf::from("src/handlers.rs"),
        symbol_id: Some(SymbolId("crate::handlers::login::Function".into())),
        ours_changeset: ChangesetId("cs-0040".into()),
        theirs_changeset: ChangesetId("cs-0042".into()),
        description: "Both agents modified handlers::login".into(),
        ours_span: Some(ConflictSpan {
            byte_range: 100..200,
            start_line: 5,
            end_line: 10,
        }),
        theirs_span: Some(ConflictSpan {
            byte_range: 100..250,
            start_line: 5,
            end_line: 12,
        }),
        base_span: Some(ConflictSpan {
            byte_range: 100..180,
            start_line: 5,
            end_line: 9,
        }),
    }
}

#[test]
fn serde_conflict_kind_roundtrip() {
    for kind in [
        ConflictKind::BothModifiedSymbol,
        ConflictKind::ModifyDeleteSymbol,
        ConflictKind::BothModifiedDependencyVersion,
        ConflictKind::RawTextConflict,
        ConflictKind::BinaryFile,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: ConflictKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}

#[test]
fn serde_conflict_detail_roundtrip() {
    let detail = sample_conflict();
    let json = serde_json::to_string(&detail).unwrap();
    let back: ConflictDetail = serde_json::from_str(&json).unwrap();
    assert_eq!(detail, back);
}

#[test]
fn span_from_byte_range_computes_lines() {
    let src = b"line1\nline2\nline3\nline4\n";
    //          0----5 6----11 12---17 18---23

    // Byte 6 is start of line 2, byte 17 is end of line 3
    let span = ConflictSpan::from_byte_range(src, 6..17);
    assert_eq!(span.start_line, 2);
    assert_eq!(span.end_line, 3);
    assert_eq!(span.byte_range, 6..17);
}

#[test]
fn span_from_byte_range_first_line() {
    let src = b"fn main() {}";
    let span = ConflictSpan::from_byte_range(src, 0..12);
    assert_eq!(span.start_line, 1);
    assert_eq!(span.end_line, 1);
}

#[test]
fn conflict_detail_without_symbol() {
    let detail = ConflictDetail {
        kind: ConflictKind::RawTextConflict,
        file: PathBuf::from("Cargo.toml"),
        symbol_id: None,
        ours_changeset: ChangesetId("cs-1".into()),
        theirs_changeset: ChangesetId("cs-2".into()),
        description: "raw text conflict in Cargo.toml".into(),
        ours_span: None,
        theirs_span: None,
        base_span: None,
    };
    let json = serde_json::to_string(&detail).unwrap();
    let back: ConflictDetail = serde_json::from_str(&json).unwrap();
    assert_eq!(detail, back);
}
