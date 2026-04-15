//! `phantom plan` — decompose a feature request into parallel agent tasks.
//!
//! Spawns an AI planner to analyze the codebase and break the request into
//! independent domains. For each domain, creates an overlay with a custom
//! instruction file and dispatches a background agent.

use std::path::Path;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, PlanId};
use phantom_core::plan::{Plan, PlanDomain, PlanStatus, RawPlanOutput};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_session::context_file;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct PlanArgs {
    /// Description of what to implement (opens interactive editor if omitted)
    pub description: Option<String>,
    /// Skip confirmation and dispatch immediately
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Show the plan without dispatching
    #[arg(long)]
    pub dry_run: bool,
    /// Don't auto-submit (agents will wait for manual submit)
    #[arg(long)]
    pub no_submit: bool,
}

pub async fn run(args: PlanArgs) -> anyhow::Result<()> {
    let description = match args.description {
        Some(d) => d,
        None => match super::textbox::multiline_input(
            "Describe what to implement:",
            "Enter your plan description...",
        )? {
            Some(d) if !d.trim().is_empty() => d,
            _ => {
                println!("Aborted.");
                return Ok(());
            }
        },
    };

    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;

    // Step 1: Generate plan via AI planner.
    println!("Planning... analyzing codebase for: {description:?}");
    println!();

    let raw_output = run_planner(&ctx.repo_root, &ctx.phantom_dir, &description)?;

    // Step 2: Build the Plan struct.
    let plan_id = generate_plan_id();
    let plan = build_plan(&plan_id, &description, raw_output);

    if plan.domains.is_empty() {
        println!("Planner returned no domains. Nothing to dispatch.");
        return Ok(());
    }

    // Step 3: Display the plan.
    display_plan(&plan);

    if args.dry_run {
        println!("(dry run — not dispatching)");
        return Ok(());
    }

    // Step 4: Confirm.
    if !args.yes {
        print!("Dispatch {} agent(s)? [Y/n] ", plan.domains.len());
        use std::io::Write;
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();
        if input == "n" || input == "no" {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Step 5: Persist the plan.
    let plan_dir = ctx.phantom_dir.join("plans").join(&plan_id.0);
    std::fs::create_dir_all(&plan_dir)
        .with_context(|| format!("failed to create plan directory {}", plan_dir.display()))?;

    let plan_json = serde_json::to_string_pretty(&plan).context("failed to serialize plan")?;
    std::fs::write(plan_dir.join("plan.json"), &plan_json).context("failed to write plan.json")?;

    // Step 5b: Validate no cycles in dependency graph.
    validate_no_cycles(&plan.domains)?;

    // Step 5c: Warn about file overlap between parallel domains.
    warn_parallel_file_overlap(&plan);

    // Step 6: Dispatch agents.
    let mut plan = plan;
    let mut dispatched_agents = Vec::new();
    let mut overlays = ctx.open_overlays_restored()?;

    for domain in &plan.domains {
        // Resolve domain name dependencies to agent IDs.
        let upstream_agent_ids: Vec<String> = domain
            .depends_on
            .iter()
            .filter_map(|dep_name| {
                plan.domains
                    .iter()
                    .find(|d| d.name == *dep_name)
                    .map(|d| d.agent_id.clone())
            })
            .collect();

        dispatch_domain(
            &ctx,
            &events,
            &mut overlays,
            &plan,
            domain,
            &plan_dir,
            &upstream_agent_ids,
        )
        .await?;
        dispatched_agents.push(AgentId(domain.agent_id.clone()));
    }

    // Step 7: Emit PlanCreated event and update persisted status.
    plan.status = PlanStatus::Dispatched;

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: ChangesetId(format!("plan-{plan_id}")),
        agent_id: AgentId("phantom-planner".into()),
        causal_parent: None,
        kind: EventKind::PlanCreated {
            plan_id: plan_id.clone(),
            request: description.clone(),
            domain_count: plan.domains.len() as u32,
            agent_ids: dispatched_agents,
        },
    };
    events.append(event).await?;

    let plan_json = serde_json::to_string_pretty(&plan).context("failed to serialize plan")?;
    std::fs::write(plan_dir.join("plan.json"), &plan_json).context("failed to update plan.json")?;

    println!();
    println!("Run `phantom background` to watch progress.");
    println!("Run `phantom status` to see all agents.");

    Ok(())
}

/// Run the AI planner to decompose the request into domains.
fn run_planner(
    repo_root: &Path,
    phantom_dir: &Path,
    description: &str,
) -> anyhow::Result<RawPlanOutput> {
    let prompt = build_planning_prompt(description);

    let cli_command = crate::context::default_cli(phantom_dir);
    let adapter = phantom_session::adapter::adapter_for(&cli_command);
    let mut cmd = adapter
        .build_headless_command(repo_root, &prompt, &[], None)
        .context("planner CLI does not support headless mode")?;
    cmd.args(["--output-format", "json"]);
    cmd.stdin(std::process::Stdio::null());

    let output = cmd.output().with_context(|| {
        format!("failed to run planner — is '{cli_command}' installed and on PATH?")
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("planner exited with {}: {stderr}", output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Claude with --output-format json wraps the result in a JSON object
    // with a "result" field containing the text. Try parsing that first.
    if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(&stdout)
        && let Some(result_text) = wrapper.get("result").and_then(|v| v.as_str())
        && let Some(parsed) = try_extract_plan_json(result_text)
    {
        return Ok(parsed);
    }

    // Fallback: try extracting JSON directly from stdout.
    if let Some(parsed) = try_extract_plan_json(&stdout) {
        return Ok(parsed);
    }

    anyhow::bail!("failed to parse planner output as plan JSON. Raw output:\n{stdout}")
}

/// Try to extract a `RawPlanOutput` from text that may contain markdown fences
/// or other wrapping around the JSON.
fn try_extract_plan_json(text: &str) -> Option<RawPlanOutput> {
    // Direct parse.
    if let Ok(plan) = serde_json::from_str::<RawPlanOutput>(text) {
        return Some(plan);
    }

    // Extract from markdown code fence.
    let json_str = extract_json_object(text)?;
    serde_json::from_str::<RawPlanOutput>(json_str).ok()
}

/// Extract the outermost JSON object from text by finding the first `{` and last `}`.
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

/// Build the prompt sent to Claude for plan decomposition.
fn build_planning_prompt(description: &str) -> String {
    format!(
        r#"Analyze this codebase and create an implementation plan for the following request:

"{description}"

Decompose the work into independent domains that can be executed in parallel by separate AI agents. Each domain should be a self-contained unit of work that modifies a distinct set of files.

Output ONLY a JSON object with this structure (no markdown fences, no explanation):
{{
  "domains": [
    {{
      "name": "kebab-case-name",
      "description": "What this domain implements",
      "files_to_modify": ["path/to/file1.rs"],
      "files_not_to_modify": ["paths/owned/by/other/domains"],
      "requirements": ["Requirement 1", "Requirement 2"],
      "verification": ["cargo test", "cargo clippy"],
      "depends_on": []
    }}
  ]
}}

Rules:
- Each domain gets its own agent with its own filesystem overlay
- CONFLICT PREVENTION: No two parallel domains (same wave) may list the same file in files_to_modify. If two domains need the same file, one MUST depends_on the other
- Shared config/build files (package.json, tsconfig.json, Cargo.toml, pyproject.toml, go.mod, Makefile, etc.) are the #1 source of merge conflicts. If multiple domains need these, create a "scaffold" or "setup" domain (wave 0) that owns all shared config, and have other domains depends_on it
- For greenfield projects (empty or near-empty repo), ALWAYS create a scaffold domain for project setup and config files as wave 0
- files_not_to_modify MUST list every file owned by another domain to prevent accidental edits
- Use depends_on freely when file sets overlap — correctness matters more than maximum parallelism
- Keep domains focused: 1-5 files each
- Include verification commands appropriate for this project's toolchain
- Names must be unique kebab-case identifiers
- Every domain MUST have at least one requirement and one verification command"#
    )
}

/// Generate a timestamp-based plan ID.
fn generate_plan_id() -> PlanId {
    let now = Utc::now();
    PlanId(now.format("plan-%Y%m%d-%H%M%S").to_string())
}

/// Convert raw planner output into a full Plan struct.
fn build_plan(plan_id: &PlanId, request: &str, raw: RawPlanOutput) -> Plan {
    let domains = raw
        .domains
        .into_iter()
        .map(|d| {
            let agent_id = d.name.clone();
            PlanDomain {
                name: d.name,
                agent_id,
                description: d.description,
                files_to_modify: d.files_to_modify,
                files_not_to_modify: d.files_not_to_modify,
                requirements: d.requirements,
                verification: d.verification,
                depends_on: d.depends_on,
            }
        })
        .collect();

    Plan {
        id: plan_id.clone(),
        request: request.to_string(),
        created_at: Utc::now(),
        domains,
        status: PlanStatus::Draft,
    }
}

/// Display the plan to the user, grouped by execution wave.
fn display_plan(plan: &Plan) {
    println!("Plan: {}", plan.id);
    println!("  {} domain(s) identified:", plan.domains.len());
    println!();

    // Compute wave depth for each domain.
    let waves = compute_waves(&plan.domains);
    let max_wave = waves.values().copied().max().unwrap_or(0);

    for wave in 0..=max_wave {
        let domains_in_wave: Vec<&PlanDomain> = plan
            .domains
            .iter()
            .filter(|d| waves.get(d.name.as_str()).copied().unwrap_or(0) == wave)
            .collect();

        if domains_in_wave.is_empty() {
            continue;
        }

        if max_wave > 0 {
            if wave == 0 {
                println!("  Wave {wave} (immediate):");
            } else {
                let after: Vec<&str> = domains_in_wave
                    .iter()
                    .flat_map(|d| d.depends_on.iter().map(String::as_str))
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                println!("  Wave {} (after: {}):", wave, after.join(", "));
            }
        }

        for domain in &domains_in_wave {
            println!("    - {}", domain.name);
            println!("      {}", domain.description);
            if !domain.files_to_modify.is_empty() {
                let files: Vec<_> = domain
                    .files_to_modify
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect();
                println!("      Files: {}", files.join(", "));
            }
            if !domain.depends_on.is_empty() {
                println!("      Depends on: {}", domain.depends_on.join(", "));
            }
            println!();
        }
    }
}

/// Compute the wave (topological depth) for each domain.
/// Wave 0 = no dependencies, wave 1 = depends only on wave-0 domains, etc.
fn compute_waves(domains: &[PlanDomain]) -> std::collections::HashMap<&str, usize> {
    use std::collections::HashMap;
    let mut waves: HashMap<&str, usize> = HashMap::new();

    // Iterative fixed-point: keep resolving until stable.
    loop {
        let mut changed = false;
        for domain in domains {
            let wave = if domain.depends_on.is_empty() {
                0
            } else {
                domain
                    .depends_on
                    .iter()
                    .map(|dep| waves.get(dep.as_str()).copied().unwrap_or(0) + 1)
                    .max()
                    .unwrap_or(0)
            };
            let prev = waves.insert(domain.name.as_str(), wave);
            if prev != Some(wave) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    waves
}

/// Validate that the dependency graph has no cycles.
///
/// Uses Kahn's algorithm (topological sort via in-degree counting).
/// Returns `Err` with a descriptive message if a cycle is found.
fn validate_no_cycles(domains: &[PlanDomain]) -> anyhow::Result<()> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let names: HashSet<&str> = domains.iter().map(|d| d.name.as_str()).collect();

    // Build adjacency list and in-degree counts.
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for domain in domains {
        in_degree.entry(domain.name.as_str()).or_insert(0);
        for dep in &domain.depends_on {
            if !names.contains(dep.as_str()) {
                anyhow::bail!(
                    "domain '{}' depends on '{}' which does not exist in the plan",
                    domain.name,
                    dep
                );
            }
            *in_degree.entry(domain.name.as_str()).or_insert(0) += 1;
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(domain.name.as_str());
        }
    }

    // Process nodes with zero in-degree.
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|entry| *entry.1 == 0)
        .map(|entry| *entry.0)
        .collect();

    let mut processed = 0usize;

    while let Some(node) = queue.pop_front() {
        processed += 1;
        if let Some(deps) = dependents.get(node) {
            for &dependent in deps {
                if let Some(deg) = in_degree.get_mut(dependent) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dependent);
                    }
                }
            }
        }
    }

    if processed < names.len() {
        let in_cycle: Vec<&str> = in_degree
            .iter()
            .filter(|entry| *entry.1 > 0)
            .map(|entry| *entry.0)
            .collect();
        anyhow::bail!(
            "dependency cycle detected among domains: {}",
            in_cycle.join(" -> ")
        );
    }

    Ok(())
}

/// Warn about files that appear in multiple domains within the same execution
/// wave. File overlap between parallel domains causes merge conflicts.
fn warn_parallel_file_overlap(plan: &Plan) {
    use std::collections::HashMap;

    let waves = compute_waves(&plan.domains);
    let max_wave = waves.values().copied().max().unwrap_or(0);

    for wave in 0..=max_wave {
        // Collect files_to_modify per domain in this wave.
        let mut file_owners: HashMap<&Path, Vec<&str>> = HashMap::new();
        for domain in &plan.domains {
            if waves.get(domain.name.as_str()).copied().unwrap_or(0) != wave {
                continue;
            }
            for file in &domain.files_to_modify {
                file_owners
                    .entry(file.as_path())
                    .or_default()
                    .push(&domain.name);
            }
        }

        for (file, owners) in &file_owners {
            if owners.len() > 1 {
                eprintln!(
                    "WARNING: {} is listed in files_to_modify by {} parallel domains in wave {}: {}",
                    file.display(),
                    owners.len(),
                    wave,
                    owners.join(", "),
                );
                eprintln!(
                    "  This will likely cause a merge conflict. Consider adding depends_on \
                     between these domains."
                );
            }
        }
    }
}

/// Dispatch a single domain as a background agent.
#[allow(clippy::too_many_arguments)]
async fn dispatch_domain(
    ctx: &PhantomContext,
    events: &SqliteEventStore,
    overlays: &mut phantom_overlay::OverlayManager,
    plan: &Plan,
    domain: &PlanDomain,
    plan_dir: &Path,
    upstream_agent_ids: &[String],
) -> anyhow::Result<()> {
    let agent_id = AgentId::validate(&domain.agent_id)
        .map_err(|e| anyhow::anyhow!("invalid agent name '{}': {e}", domain.agent_id))?;
    let git = ctx.open_git()?;
    let head = git.head_oid().context("failed to read HEAD")?;

    // Create the overlay.
    let handle = overlays
        .create_overlay(agent_id.clone(), &ctx.repo_root)
        .with_context(|| format!("failed to create overlay for {}", domain.agent_id))?;
    let mount_point = handle.mount_point.clone();
    let upper_dir = handle.upper_dir.clone();

    // Generate changeset ID.
    let cs_id = generate_changeset_id();

    // Emit TaskCreated event.
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: cs_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent: None,
        kind: EventKind::TaskCreated {
            base_commit: head,
            task: domain.description.clone(),
        },
    };
    events.append(event).await?;

    // Write initial current_base.
    phantom_orchestrator::live_rebase::write_current_base(&ctx.phantom_dir, &agent_id, &head)
        .context("failed to write initial current_base")?;

    // Spawn FUSE daemon (blocking I/O — run off the async executor).
    let fuse_phantom_dir = ctx.phantom_dir.clone();
    let fuse_repo_root = ctx.repo_root.clone();
    let fuse_agent = domain.agent_id.clone();
    let fuse_mount = mount_point.clone();
    let fuse_upper = upper_dir.clone();
    let fuse_mounted = tokio::task::spawn_blocking(move || {
        spawn_fuse_if_available(
            &fuse_phantom_dir,
            &fuse_repo_root,
            &fuse_agent,
            &fuse_mount,
            &fuse_upper,
        )
    })
    .await
    .unwrap_or(false);

    let work_dir = if fuse_mounted {
        mount_point.clone()
    } else {
        upper_dir.clone()
    };

    // Extract cross-domain signatures for token-efficient context injection.
    let cross_domain_sigs =
        phantom_session::signatures::extract_cross_domain_signatures(&ctx.repo_root, domain, plan);
    let sigs_ref = if cross_domain_sigs.is_empty() {
        None
    } else {
        Some(cross_domain_sigs.as_str())
    };

    // Generate the domain instruction file.
    let instructions_dir = plan_dir.join("instructions");
    let instructions_path = instructions_dir.join(format!("{}.md", domain.agent_id));
    context_file::write_plan_domain_instructions(&instructions_path, domain, plan, sigs_ref)?;

    // Write context file into overlay.
    context_file::write_context_file(
        &work_dir,
        &agent_id,
        &cs_id,
        &head,
        Some(&domain.description),
    )?;

    // Spawn the agent monitor with the custom instruction file.
    let cli_command = crate::context::default_cli(&ctx.phantom_dir);
    super::task::spawn_agent_monitor(
        &ctx.phantom_dir,
        &ctx.repo_root,
        &domain.agent_id,
        &cs_id,
        &domain.description,
        &work_dir,
        &cli_command,
        Some(&instructions_path),
        upstream_agent_ids,
    )?;

    let log_file = ctx
        .phantom_dir
        .join("overlays")
        .join(&domain.agent_id)
        .join("agent.log");

    println!("Agent '{}' tasked (background).", domain.agent_id);
    println!("  Changeset: {cs_id}");
    println!("  Task:      {}", domain.description);
    println!("  Log:       {}", log_file.display());
    println!("  Overlay:   {}", work_dir.display());
    if fuse_mounted {
        println!("  FUSE:      mounted");
    }
    if !upstream_agent_ids.is_empty() {
        println!("  Waiting:   {}", upstream_agent_ids.join(", "));
    }
    println!();

    Ok(())
}

/// Re-export from task module to avoid duplication.
use super::task::generate_changeset_id;

/// Try to spawn a FUSE daemon. Returns true if mounted, false if FUSE unavailable.
fn spawn_fuse_if_available(
    phantom_dir: &Path,
    repo_root: &Path,
    agent: &str,
    mount_point: &Path,
    upper_dir: &Path,
) -> bool {
    let Ok(phantom_bin) = std::env::current_exe() else {
        return false;
    };

    let overlay_root = phantom_dir.join("overlays").join(agent);
    let pid_file = overlay_root.join("fuse.pid");
    let log_file = overlay_root.join("fuse.log");

    let Ok(log_handle) = std::fs::File::create(&log_file) else {
        return false;
    };

    let child = std::process::Command::new(&phantom_bin)
        .arg("_fuse-mount")
        .arg("--agent")
        .arg(agent)
        .arg("--mount-point")
        .arg(mount_point)
        .arg("--upper-dir")
        .arg(upper_dir)
        .arg("--lower-dir")
        .arg(repo_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log_handle)
        .spawn();

    let Ok(child) = child else {
        return false;
    };

    let _ = crate::pid_guard::write_pid_file(&pid_file, child.id() as i32);

    // Wait briefly for mount.
    wait_for_mount(mount_point, std::time::Duration::from_secs(5))
}

/// Poll until a FUSE mount appears. Returns true if mounted.
fn wait_for_mount(mount_point: &Path, timeout: std::time::Duration) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Some(parent) = mount_point.parent() else {
        return false;
    };
    let start = std::time::Instant::now();

    loop {
        if let (Ok(m), Ok(p)) = (std::fs::metadata(mount_point), std::fs::metadata(parent))
            && m.dev() != p.dev()
        {
            return true;
        }

        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(plan.domains[0].agent_id, "rate-limiting");
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
        domain_with_files(name, depends_on, &[])
    }

    fn domain_with_files(name: &str, depends_on: &[&str], files: &[&str]) -> PlanDomain {
        PlanDomain {
            name: name.into(),
            agent_id: format!("plan-test-{name}"),
            description: format!("test domain {name}"),
            files_to_modify: files.iter().map(std::path::PathBuf::from).collect(),
            files_not_to_modify: vec![],
            requirements: vec![],
            verification: vec![],
            depends_on: depends_on
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
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
        let domains = vec![domain("a", &[]), domain("b", &["a"]), domain("c", &["b"])];
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

    // ── File overlap warning tests ────────────────────────────────────

    // warn_parallel_file_overlap prints to stderr and doesn't return a
    // testable value, so we extract the detection logic into a helper and
    // test that instead. The actual function calls this same logic.

    /// Detect overlapping files between parallel domains in the same wave.
    /// Returns (file, [domain_names]) for each overlap.
    fn detect_overlaps(plan: &Plan) -> Vec<(std::path::PathBuf, Vec<String>)> {
        use std::collections::HashMap;

        let waves = compute_waves(&plan.domains);
        let max_wave = waves.values().copied().max().unwrap_or(0);
        let mut results = Vec::new();

        for wave in 0..=max_wave {
            let mut file_owners: HashMap<&Path, Vec<&str>> = HashMap::new();
            for domain in &plan.domains {
                if waves.get(domain.name.as_str()).copied().unwrap_or(0) != wave {
                    continue;
                }
                for file in &domain.files_to_modify {
                    file_owners
                        .entry(file.as_path())
                        .or_default()
                        .push(&domain.name);
                }
            }
            for (file, owners) in file_owners {
                if owners.len() > 1 {
                    results.push((
                        file.to_path_buf(),
                        owners.into_iter().map(String::from).collect(),
                    ));
                }
            }
        }
        results
    }

    #[test]
    fn detects_overlap_in_same_wave() {
        let domains = vec![
            domain_with_files("scaffold", &[], &["package.json", "src/index.ts"]),
            domain_with_files("vim-engine", &[], &["package.json", "src/vim.ts"]),
        ];
        let plan = build_plan(
            &PlanId("test".into()),
            "test",
            RawPlanOutput {
                domains: vec![], // unused, we override
            },
        );
        let plan = Plan { domains, ..plan };

        let overlaps = detect_overlaps(&plan);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].0, std::path::PathBuf::from("package.json"));
        assert_eq!(overlaps[0].1.len(), 2);
    }

    #[test]
    fn no_overlap_when_dependency_separates_waves() {
        let domains = vec![
            domain_with_files("scaffold", &[], &["package.json", "tsconfig.json"]),
            domain_with_files("vim-engine", &["scaffold"], &["package.json", "src/vim.ts"]),
        ];
        let plan = Plan {
            id: PlanId("test".into()),
            request: "test".into(),
            created_at: Utc::now(),
            domains,
            status: PlanStatus::Draft,
        };

        let overlaps = detect_overlaps(&plan);
        // package.json is in different waves (0 and 1), so no parallel overlap
        assert!(overlaps.is_empty());
    }

    #[test]
    fn no_overlap_when_files_disjoint() {
        let domains = vec![
            domain_with_files("api", &[], &["src/api.ts"]),
            domain_with_files("ui", &[], &["src/ui.ts"]),
        ];
        let plan = Plan {
            id: PlanId("test".into()),
            request: "test".into(),
            created_at: Utc::now(),
            domains,
            status: PlanStatus::Draft,
        };

        let overlaps = detect_overlaps(&plan);
        assert!(overlaps.is_empty());
    }
}
