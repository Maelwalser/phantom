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
    override_help = "\
Semantic version control for agentic AI development

Usage: ph <agent> [OPTIONS]
       ph [COMMAND]

Commands:
  init            Initialize Phantom in an existing git repository
  submit/sub      Submit an agent's work: merge to trunk and ripple to other agents
  status/st       Show status of overlays and changesets
  tasks/t         List all agent task overlays
  plan            Decompose a feature into parallel agent tasks
  resolve/res     Auto-resolve merge conflicts by launching an AI agent
  rollback/rb     Roll back a changeset and replay downstream
  log/l           Query the event log
  changes/c       Show materializations, or submits for a specific agent
  destroy/rm      Destroy an agent's overlay
  resume/re       Select and resume an interactive agent session
  background/b    Watch background agents in real-time
  exec/x          Run a command inside an agent's overlay
  down            Tear down Phantom: unmount all FUSE overlays and remove .phantom/
  help            Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version"
)]
pub struct Cli {
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
    /// Auto-resolve merge conflicts by launching an AI agent
    #[command(alias = "res", display_name = "resolve/res")]
    Resolve(commands::resolve::ResolveArgs),
    /// Roll back a changeset and replay downstream
    #[command(alias = "rb", display_name = "rollback/rb")]
    Rollback(commands::rollback::RollbackArgs),
    /// Query the event log
    #[command(alias = "l", display_name = "log/l")]
    Log(commands::log::LogArgs),
    /// Show materializations, or submits for a specific agent
    #[command(alias = "c", display_name = "changes/c")]
    Changes(commands::changes::ChangesArgs),
    /// Destroy an agent's overlay
    #[command(alias = "rm", display_name = "destroy/rm")]
    Destroy(commands::destroy::DestroyArgs),
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

    /// Internal: run FUSE mount daemon (not for direct use)
    #[command(name = "_fuse-mount", hide = true)]
    FuseMount(commands::fuse_mount::FuseMountArgs),

    /// Internal: monitor a background agent process (not for direct use)
    #[command(name = "_agent-monitor", hide = true)]
    AgentMonitor(commands::agent_monitor::AgentMonitorArgs),

    /// Catch-all: treat unrecognized subcommands as agent names for task creation
    #[command(external_subcommand)]
    ExternalTask(Vec<OsString>),
}
