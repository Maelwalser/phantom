use super::*;
use phantom_core::event::EventKind;
use phantom_core::id::GitOid;

#[test]
fn materialized_prefix_matches_serialized_format() {
    let kind = EventKind::ChangesetMaterialized {
        new_commit: GitOid::zero(),
    };
    let json = serde_json::to_string(&kind).unwrap();
    let prefix = MATERIALIZED_KIND_PREFIX.trim_end_matches('%');
    assert!(
        json.starts_with(prefix),
        "MATERIALIZED_KIND_PREFIX is stale: got {json}"
    );
}
