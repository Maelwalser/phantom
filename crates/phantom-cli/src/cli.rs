//! Clap argument definitions: `Cli`, `Commands`, and the `TaskWrapper` used
//! for the external-subcommand fallback.

use std::ffi::OsString;

use clap::Parser;

use crate::commands;

/// Create or resume an agent task overlay.
#[derive(Parser)]
#[command(name = "ph", about = "Create or resume an agent task overlay")]
pub struct TaskWrapper {
    #[command(flatten)]
    pub args: commands::task::TaskArgs,
}

#[derive(Parser)]
#[command(
    name = "ph",
    version,
    about = "Semantic version control for agentic AI development",
    disable_help_flag = true
)]
pub struct Cli {
    /// Print help
    #[arg(short = 'h', long = "help", global = true, action = clap::ArgAction::HelpShort)]
    pub help: Option<bool>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(clap::Subcommand)]
pub enum Commands {
    /// Initialize Phantom in an existing git repository
    Init,
    /// Submit an agent's work: merge to trunk and ripple to other agents
    #[command(alias = "sub", display_name = "submit/sub")]
    Submit(commands::submit::SubmitArgs),
    /// Show status of overlays and changesets
    #[command(alias = "st", display_name = "status/st")]
    Status(commands::status::StatusArgs),
    /// List all agent task overlays
    #[command(alias = "t", display_name = "tasks/t")]
    Tasks(commands::tasks::TasksArgs),
    /// Decompose a feature into parallel agent tasks
    Plan(commands::plan::PlanArgs),
    /// Inspect conflicted changesets (read-only) for manual resolution
    #[command(alias = "conf", display_name = "conflicts/conf")]
    Conflicts(commands::conflicts::ConflictsArgs),
    /// Auto-resolve merge conflicts by launching an AI agent
    #[command(alias = "res", display_name = "resolve/res")]
    Resolve(commands::resolve::ResolveArgs),
    /// Roll back a changeset and replay downstream
    #[command(alias = "rb", display_name = "rollback/rb")]
    Rollback(commands::rollback::RollbackArgs),
    /// Reconcile orphan pre-commit fences after a crashed submit
    Recover(commands::recover::RecoverArgs),
    /// Query the event log
    #[command(alias = "l", display_name = "log/l")]
    Log(commands::log::LogArgs),
    /// Show materializations, or submits for a specific agent
    #[command(alias = "c", display_name = "changes/c")]
    Changes(commands::changes::ChangesArgs),
    /// Remove an agent's overlay (immediate, no prompt)
    #[command(alias = "rm", display_name = "remove/rm")]
    Remove(commands::remove::RemoveArgs),
    /// Watch background agents in real-time
    #[command(alias = "b", display_name = "background/b")]
    Background(commands::background::BackgroundArgs),
    /// Select and resume an interactive agent session
    #[command(alias = "re", display_name = "resume/re")]
    Resume(commands::resume::ResumeArgs),
    /// Tear down Phantom: unmount all FUSE overlays and remove .phantom/
    Down(commands::down::DownArgs),

    /// Run a command inside an agent's overlay
    #[command(alias = "x", display_name = "exec/x")]
    Exec(commands::exec::ExecArgs),

    /// Run the project's build, lint, and test commands as a post-plan gate
    #[command(alias = "v", display_name = "verify/v")]
    Verify(commands::verify::VerifyArgs),

    /// Internal: run FUSE mount daemon (not for direct use)
    #[command(name = "_fuse-mount", hide = true)]
    FuseMount(commands::fuse_mount::FuseMountArgs),

    /// Internal: monitor a background agent process (not for direct use)
    #[command(name = "_agent-monitor", hide = true)]
    AgentMonitor(commands::agent_monitor::AgentMonitorArgs),

    /// Internal: emit pending trunk-update notifications as Claude hook output
    #[command(name = "_notify-hook", hide = true)]
    NotifyHook(commands::notify_hook::NotifyHookArgs),

    /// Catch-all: treat unrecognized subcommands as agent names for task creation
    #[command(external_subcommand)]
    ExternalTask(Vec<OsString>),
}
