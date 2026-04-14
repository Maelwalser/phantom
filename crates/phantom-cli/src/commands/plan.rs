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
    /// Don't auto-materialize (just auto-submit)
    #[arg(long)]
    pub no_materialize: bool,
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
    println!("Planning... analyzing codebase for: {:?}", description);
    println!();

    let raw_output = run_planner(&ctx.repo_root, &description)?;

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

    // Step 6: Dispatch agents.
    let mut plan = plan;
    let auto_materialize = !args.no_materialize;
    let mut dispatched_agents = Vec::new();
    let mut overlays = ctx.open_overlays_restored()?;

    for domain in &plan.domains {
        dispatch_domain(
            &ctx,
            &events,
            &mut overlays,
            &plan,
            domain,
            &plan_dir,
            auto_materialize,
        )
        .await?;
        dispatched_agents.push(AgentId(domain.agent_id.clone()));
    }

    // Step 7: Emit PlanCreated event and update persisted status.
    plan.status = PlanStatus::Dispatched;

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: ChangesetId(format!("plan-{}", plan_id)),
        agent_id: AgentId("phantom-planner".into()),
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
fn run_planner(repo_root: &Path, description: &str) -> anyhow::Result<RawPlanOutput> {
    let prompt = build_planning_prompt(description);

    let mut cmd = std::process::Command::new("claude");
    cmd.current_dir(repo_root);
    cmd.args(["-p", &prompt]);
    cmd.args(["--output-format", "json"]);
    cmd.args(["--allowedTools", "Read", "Bash", "Glob", "Grep"]);
    cmd.stdin(std::process::Stdio::null());

    let output = cmd
        .output()
        .context("failed to run claude planner — is 'claude' installed and on PATH?")?;

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
- Minimize file overlap between domains — less overlap means fewer merge conflicts
- Keep domains focused: 1-5 files each
- Use depends_on sparingly; prefer independent domains
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
            let agent_id = format!("{}-{}", plan_id, d.name);
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

/// Display the plan to the user.
fn display_plan(plan: &Plan) {
    println!("Plan: {}", plan.id);
    println!("  {} domain(s) identified:", plan.domains.len());
    println!();

    for (i, domain) in plan.domains.iter().enumerate() {
        println!("  {}. {}", i + 1, domain.name);
        println!("     {}", domain.description);
        if !domain.files_to_modify.is_empty() {
            let files: Vec<_> = domain
                .files_to_modify
                .iter()
                .map(|f| f.display().to_string())
                .collect();
            println!("     Files: {}", files.join(", "));
        }
        if !domain.depends_on.is_empty() {
            println!("     Depends on: {}", domain.depends_on.join(", "));
        }
        println!();
    }
}

/// Dispatch a single domain as a background agent.
async fn dispatch_domain(
    ctx: &PhantomContext,
    events: &SqliteEventStore,
    overlays: &mut phantom_overlay::OverlayManager,
    plan: &Plan,
    domain: &PlanDomain,
    plan_dir: &Path,
    auto_materialize: bool,
) -> anyhow::Result<()> {
    let agent_id = AgentId(domain.agent_id.clone());
    let git = ctx.open_git()?;
    let head = git.head_oid().context("failed to read HEAD")?;

    // Create the overlay.
    let handle = overlays
        .create_overlay(agent_id.clone(), &ctx.repo_root)
        .with_context(|| format!("failed to create overlay for {}", domain.agent_id))?;
    let mount_point = handle.mount_point.clone();
    let upper_dir = handle.upper_dir.clone();

    // Generate changeset ID.
    let cs_id = generate_changeset_id(events).await?;

    // Emit TaskCreated event.
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: cs_id.clone(),
        agent_id: agent_id.clone(),
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
    super::task::spawn_agent_monitor(
        &ctx.phantom_dir,
        &ctx.repo_root,
        &domain.agent_id,
        &cs_id,
        &domain.description,
        &work_dir,
        auto_materialize,
        Some(&instructions_path),
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
    let phantom_bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let overlay_root = phantom_dir.join("overlays").join(agent);
    let pid_file = overlay_root.join("fuse.pid");
    let log_file = overlay_root.join("fuse.log");

    let log_handle = match std::fs::File::create(&log_file) {
        Ok(h) => h,
        Err(_) => return false,
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

    let child = match child {
        Ok(c) => c,
        Err(_) => return false,
    };

    let _ = std::fs::write(&pid_file, child.id().to_string());

    // Wait briefly for mount.
    wait_for_mount(mount_point, std::time::Duration::from_secs(5))
}

/// Poll until a FUSE mount appears. Returns true if mounted.
fn wait_for_mount(mount_point: &Path, timeout: std::time::Duration) -> bool {
    use std::os::unix::fs::MetadataExt;

    let parent = match mount_point.parent() {
        Some(p) => p,
        None => return false,
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
#[path = "plan_tests.rs"]
mod tests;
