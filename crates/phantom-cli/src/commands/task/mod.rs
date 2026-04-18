//! `phantom <agent>` — create an agent overlay and launch a Claude Code session.
//!
//! By default, tasking opens an interactive Claude Code CLI inside the
//! overlay's FUSE mount point, which provides a merged view of trunk + agent
//! writes. Use `--background` to create the overlay without launching a
//! session (for scripted / headless agents).
//!
//! If an overlay already exists for the agent, the command resumes the existing
//! session (reuses changeset ID, skips event emission, re-mounts FUSE if needed).

mod category;
mod resume;
mod spawn;

use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::EventId;
use phantom_core::traits::EventStore;
use phantom_overlay::OverlayError;

use crate::context::PhantomContext;

pub(crate) use resume::generate_changeset_id;
pub(crate) use spawn::spawn_agent_monitor;

#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags — each bool is independent.
pub struct TaskArgs {
    /// Agent identifier (e.g. "agent-a")
    pub agent: String,
    /// Task description for the agent (only available with --background)
    #[arg(long, requires = "background")]
    pub task: Option<String>,
    /// Create the overlay without launching a CLI session (for scripted agents)
    #[arg(long, short = 'b')]
    pub background: bool,
    /// Automatically submit and merge to trunk when the session exits.
    /// Always enabled for background agents.
    #[arg(long, alias = "auto-materialize")]
    pub auto_submit: bool,
    /// Custom command to run instead of `claude` (e.g. for testing)
    #[arg(long, conflicts_with = "background")]
    pub command: Option<String>,
    /// Skip FUSE mounting (agent works via OverlayLayer API only, no filesystem isolation)
    #[arg(long)]
    pub no_fuse: bool,
    /// Tag the task with a maintenance category OR point at a custom `.md`
    /// rules file. Built-in names (corrective / perfective / preventive /
    /// adaptive) resolve to the matching `.phantom/rules/<name>.md` rule
    /// set. Any other value is treated as a path to a markdown file, which
    /// is injected verbatim via `--append-system-prompt-file`.
    ///
    /// Passing `--category` with no value opens an interactive menu of the
    /// four built-ins.
    #[arg(
        long,
        visible_alias = "cat",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "custom",
    )]
    pub category: Option<String>,
    /// Open a multiline textbox to author a bespoke rule body for this task.
    /// The typed markdown is saved to `.phantom/rules/custom/<agent>.md` and
    /// injected via `--append-system-prompt-file`. Conflicts with
    /// `--category`.
    #[arg(long, conflicts_with = "category")]
    pub custom: bool,
}

pub async fn run(args: TaskArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let git = ctx.open_git()?;
    let events = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    let agent_id = crate::services::validate::agent_id(&args.agent)?;
    let head = git.head_oid().context("failed to read HEAD")?;
    crate::context::require_initialized_head(&head)?;

    // Resolve the user's category choice before creating the overlay. If the
    // user cancels an interactive menu or textbox, exit cleanly without
    // leaving a partial overlay behind. For resume flows where no explicit
    // flag was passed, check for a previously-saved custom rule file.
    let resolved_category = match category::resolve_category(&args, &ctx.repo_root) {
        Ok(Some(r)) => Some(r),
        Ok(None) => category::implicit_resume_from_custom(&ctx.phantom_dir, &args.agent),
        Err(e) if e.to_string() == category::CANCELLED => {
            println!("  {}", console::style("Cancelled.").dim());
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // Try to create a new overlay. If one already exists, switch to resume mode.
    let (changeset_id, base_commit, is_new, mount_point, upper_dir) =
        match overlays.create_overlay(agent_id.clone(), &ctx.repo_root) {
            Ok(handle) => {
                let mount_point = handle.mount_point.clone();
                let upper_dir = handle.upper_dir.clone();

                let cs_id = generate_changeset_id();

                let event = Event {
                    id: EventId(0),
                    timestamp: Utc::now(),
                    changeset_id: cs_id.clone(),
                    agent_id: agent_id.clone(),
                    causal_parent: None,
                    kind: EventKind::TaskCreated {
                        base_commit: head,
                        task: args.task.clone().unwrap_or_default(),
                    },
                };
                events.append(event).await?;

                phantom_orchestrator::live_rebase::write_current_base(
                    &ctx.phantom_dir,
                    &agent_id,
                    &head,
                )
                .context("failed to write initial current_base")?;

                (cs_id, head, true, mount_point, upper_dir)
            }
            Err(OverlayError::AlreadyExists(_)) => {
                let (old_cs_id, old_base) =
                    resume::recover_changeset_from_events(&events, &agent_id).await?;
                let resume_status = resume::check_changeset_resumable(&events, &old_cs_id).await?;

                let upper_dir = overlays.upper_dir(&agent_id)?.to_path_buf();
                let mount_point = ctx
                    .phantom_dir
                    .join("overlays")
                    .join(&agent_id.0)
                    .join("mount");

                // If the previous changeset was materialized, start a new one
                // so the agent can continue working on the same overlay.
                let (cs_id, base) = match resume_status {
                    resume::ResumeStatus::Materialized => {
                        let new_cs_id = generate_changeset_id();
                        let event = Event {
                            id: EventId(0),
                            timestamp: Utc::now(),
                            changeset_id: new_cs_id.clone(),
                            agent_id: agent_id.clone(),
                            causal_parent: None,
                            kind: EventKind::TaskCreated {
                                base_commit: head,
                                task: args.task.clone().unwrap_or_default(),
                            },
                        };
                        events.append(event).await?;

                        phantom_orchestrator::live_rebase::write_current_base(
                            &ctx.phantom_dir,
                            &agent_id,
                            &head,
                        )
                        .context("failed to write current_base for new changeset")?;

                        (new_cs_id, head)
                    }
                    resume::ResumeStatus::Submitted | resume::ResumeStatus::InProgress => {
                        (old_cs_id, old_base)
                    }
                };

                (cs_id, base, false, mount_point, upper_dir)
            }
            Err(e) => return Err(e.into()),
        };

    // Spawn FUSE daemon unless --no-fuse or already mounted.
    let already_mounted = crate::fs::fuse::is_mounted(&mount_point);
    let fuse_mounted = if args.no_fuse || already_mounted {
        already_mounted
    } else {
        crate::fs::fuse::spawn_daemon(
            &ctx.phantom_dir,
            &ctx.repo_root,
            &args.agent,
            &mount_point,
            &upper_dir,
            &crate::fs::fuse::FsOverrides::default(),
            Duration::from_secs(5),
        )?;
        true
    };

    // The agent's working directory: FUSE mount (merged view) or upper dir (writes only).
    let work_dir = if fuse_mounted {
        mount_point.clone()
    } else {
        upper_dir.clone()
    };

    let base_short = base_commit.to_hex().chars().take(12).collect::<String>();

    // Materialise the resolved category into a concrete rules-file path on
    // disk. `Builtin` ensures the four static files exist; `File` passes the
    // user-supplied path through unchanged; `Inline` writes the textbox body
    // to `.phantom/rules/custom/<agent>.md`.
    let category_rules_path = resolved_category
        .as_ref()
        .map(|r| r.materialise(&ctx.phantom_dir, &args.agent))
        .transpose()
        .context("failed to prepare category rules file")?;

    if args.background {
        let task = match args.task {
            Some(t) => t,
            None => match crate::ui::textbox::multiline_input(
                "Describe the task for this agent:",
                "Enter task description...",
            )? {
                Some(d) if !d.trim().is_empty() => d,
                _ => {
                    println!("Aborted.");
                    return Ok(());
                }
            },
        };

        // Detect the repo's toolchain once so the agent sees concrete
        // verification commands instead of abstract verbs. Empty toolchains
        // render no block — backwards-compatible.
        let detector = phantom_toolchain::ToolchainDetector::new();
        let toolchain = detector.detect_repo_root(&ctx.repo_root);

        phantom_session::context_file::write_context_file_with_toolchain(
            &work_dir,
            &agent_id,
            &changeset_id,
            &base_commit,
            Some(&task),
            Some(&toolchain),
        )?;

        // Spawn the monitor process, which in turn spawns the agent CLI as its
        // child. This ensures the monitor can waitpid for the real exit code.
        let log_file = ctx
            .phantom_dir
            .join("overlays")
            .join(&args.agent)
            .join("agent.log");
        let config_default = crate::context::default_cli(&ctx.phantom_dir);
        let cli_command = args.command.as_deref().unwrap_or(&config_default);
        spawn_agent_monitor(
            &ctx.phantom_dir,
            &ctx.repo_root,
            &args.agent,
            &changeset_id,
            &task,
            &work_dir,
            cli_command,
            category_rules_path.as_deref(),
            &[],
        )?;

        let verb_styled = if is_new {
            console::style("tasked").green()
        } else {
            console::style("resumed").cyan()
        };
        println!(
            "  Agent '{}' {verb_styled} {}",
            console::style(&args.agent).bold(),
            console::style("(background)").dim()
        );
        crate::ui::key_value("Changeset", changeset_id.to_string());
        crate::ui::key_value("Task", task);
        crate::ui::key_value("Log", log_file.display());
        crate::ui::key_value("Overlay", work_dir.display());
        crate::ui::key_value("Base", console::style(&base_short).cyan());
        if let Some(resolved) = resolved_category.as_ref() {
            crate::ui::key_value("Category", console::style(resolved.display_label()).cyan());
        }
        if fuse_mounted {
            crate::ui::key_value("FUSE", console::style("mounted").green());
        }
        println!();
        println!(
            "  Run {} again to check progress.",
            console::style(format!("ph {}", args.agent)).bold()
        );
    } else {
        // If a background agent is already running or has completed for this
        // overlay, show its status instead of opening an interactive session.
        // This prevents accidentally launching a second CLI on top of a
        // background agent's work.
        if !is_new && resume::has_background_agent(&ctx.phantom_dir, &args.agent) {
            // Delegate to the detailed status view.
            super::status::run(super::status::StatusArgs {
                agent: Some(args.agent.clone()),
            })
            .await?;
            return Ok(());
        }

        if is_new {
            println!(
                "  Agent '{}' {}.",
                console::style(&args.agent).bold(),
                console::style("tasked").green()
            );
            crate::ui::key_value("Changeset", changeset_id.to_string());
            crate::ui::key_value("Overlay", work_dir.display());
            crate::ui::key_value("Base", console::style(&base_short).cyan());
            if fuse_mounted {
                crate::ui::key_value("FUSE", console::style("mounted").green());
            }
            println!();
        } else {
            println!(
                "  Task '{}' {}.",
                console::style(&args.agent).bold(),
                console::style("resumed").cyan()
            );
        }
        super::interactive::run_interactive_session(
            &ctx,
            &agent_id,
            &changeset_id,
            &base_commit,
            &work_dir,
            &args,
            category_rules_path.as_deref(),
        )
        .await?;
    }

    Ok(())
}
