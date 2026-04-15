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

// ── Cycle detection tests ──────────────────────────────────────────

fn domain(name: &str, depends_on: &[&str]) -> PlanDomain {
    PlanDomain {
        name: name.into(),
        agent_id: format!("plan-test-{name}"),
        description: format!("test domain {name}"),
        files_to_modify: vec![],
        files_not_to_modify: vec![],
        requirements: vec![],
        verification: vec![],
        depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn validate_no_cycles_accepts_valid_dag() {
    let domains = vec![
        domain("a", &[]),
        domain("b", &["a"]),
        domain("c", &["a", "b"]),
    ];
    assert!(validate_no_cycles(&domains).is_ok());
}

#[test]
fn validate_no_cycles_accepts_independent_domains() {
    let domains = vec![domain("a", &[]), domain("b", &[]), domain("c", &[])];
    assert!(validate_no_cycles(&domains).is_ok());
}

#[test]
fn validate_no_cycles_detects_direct_cycle() {
    let domains = vec![domain("a", &["b"]), domain("b", &["a"])];
    let err = validate_no_cycles(&domains).unwrap_err();
    assert!(err.to_string().contains("cycle"));
}

#[test]
fn validate_no_cycles_detects_indirect_cycle() {
    let domains = vec![
        domain("a", &["c"]),
        domain("b", &["a"]),
        domain("c", &["b"]),
    ];
    let err = validate_no_cycles(&domains).unwrap_err();
    assert!(err.to_string().contains("cycle"));
}

#[test]
fn validate_no_cycles_detects_self_cycle() {
    let domains = vec![domain("a", &["a"])];
    let err = validate_no_cycles(&domains).unwrap_err();
    assert!(err.to_string().contains("cycle"));
}

#[test]
fn validate_no_cycles_detects_missing_dependency() {
    let domains = vec![domain("a", &["nonexistent"])];
    let err = validate_no_cycles(&domains).unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn validate_no_cycles_accepts_diamond_dag() {
    // a -> b, a -> c, b -> d, c -> d
    let domains = vec![
        domain("a", &[]),
        domain("b", &["a"]),
        domain("c", &["a"]),
        domain("d", &["b", "c"]),
    ];
    assert!(validate_no_cycles(&domains).is_ok());
}

// ── Wave computation tests ─────────────────────────────────────────

#[test]
fn compute_waves_independent_domains() {
    let domains = vec![domain("a", &[]), domain("b", &[])];
    let waves = compute_waves(&domains);
    assert_eq!(waves["a"], 0);
    assert_eq!(waves["b"], 0);
}

#[test]
fn compute_waves_linear_chain() {
    let domains = vec![
        domain("a", &[]),
        domain("b", &["a"]),
        domain("c", &["b"]),
    ];
    let waves = compute_waves(&domains);
    assert_eq!(waves["a"], 0);
    assert_eq!(waves["b"], 1);
    assert_eq!(waves["c"], 2);
}

#[test]
fn compute_waves_diamond() {
    let domains = vec![
        domain("a", &[]),
        domain("b", &["a"]),
        domain("c", &["a"]),
        domain("d", &["b", "c"]),
    ];
    let waves = compute_waves(&domains);
    assert_eq!(waves["a"], 0);
    assert_eq!(waves["b"], 1);
    assert_eq!(waves["c"], 1);
    assert_eq!(waves["d"], 2);
}
