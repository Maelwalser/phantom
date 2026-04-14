use super::*;

#[test]
fn raw_plan_output_deserializes() {
    let json = r#"{
        "domains": [
            {
                "name": "rate-limiting",
                "description": "Add rate limiting middleware",
                "files_to_modify": ["src/middleware.rs"],
                "requirements": ["Token bucket algorithm"],
                "verification": ["cargo test"]
            }
        ]
    }"#;
    let output: RawPlanOutput = serde_json::from_str(json).unwrap();
    assert_eq!(output.domains.len(), 1);
    assert_eq!(output.domains[0].name, "rate-limiting");
}

#[test]
fn plan_serde_roundtrip() {
    let plan = Plan {
        id: PlanId("plan-20260413-143022".into()),
        request: "add caching".into(),
        created_at: Utc::now(),
        domains: vec![PlanDomain {
            name: "cache".into(),
            agent_id: "plan-20260413-cache".into(),
            description: "Add cache layer".into(),
            files_to_modify: vec!["src/cache.rs".into()],
            files_not_to_modify: vec![],
            requirements: vec!["LRU cache".into()],
            verification: vec!["cargo test".into()],
            depends_on: vec![],
        }],
        status: PlanStatus::Draft,
    };
    let json = serde_json::to_string(&plan).unwrap();
    let back: Plan = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, plan.id);
    assert_eq!(back.domains.len(), 1);
}
