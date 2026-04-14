use super::*;

#[test]
fn extract_json_object_direct() {
    let text = r#"{"domains": [{"name": "test", "description": "d", "requirements": [], "verification": []}]}"#;
    let result = try_extract_plan_json(text);
    assert!(result.is_some());
    assert_eq!(result.unwrap().domains[0].name, "test");
}

#[test]
fn extract_json_from_markdown_fence() {
    let text = "Here's the plan:\n```json\n{\"domains\": [{\"name\": \"cache\", \"description\": \"add cache\", \"requirements\": [\"r1\"], \"verification\": [\"v1\"]}]}\n```\n";
    let result = try_extract_plan_json(text);
    assert!(result.is_some());
    assert_eq!(result.unwrap().domains[0].name, "cache");
}

#[test]
fn extract_json_with_surrounding_text() {
    let text = "I'll create this plan: {\"domains\": [{\"name\": \"api\", \"description\": \"d\", \"requirements\": [], \"verification\": []}]} That should work.";
    let result = try_extract_plan_json(text);
    assert!(result.is_some());
}

#[test]
fn build_plan_assigns_agent_ids() {
    let raw = RawPlanOutput {
        domains: vec![phantom_core::plan::RawPlanDomain {
            name: "rate-limiting".into(),
            description: "add rate limiting".into(),
            files_to_modify: vec!["src/lib.rs".into()],
            files_not_to_modify: vec![],
            requirements: vec!["impl token bucket".into()],
            verification: vec!["cargo test".into()],
            depends_on: vec![],
        }],
    };
    let plan_id = PlanId("plan-20260413-143022".into());
    let plan = build_plan(&plan_id, "test", raw);
    assert_eq!(
        plan.domains[0].agent_id,
        "plan-20260413-143022-rate-limiting"
    );
    assert_eq!(plan.status, PlanStatus::Draft);
}

#[test]
fn generate_plan_id_has_expected_format() {
    let id = generate_plan_id();
    assert!(id.0.starts_with("plan-"));
    assert!(id.0.len() > 10);
}
