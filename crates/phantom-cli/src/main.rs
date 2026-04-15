// PID guard, FUSE mount, and TTY detection require libc calls.
#![allow(unsafe_code)]
use std::ffi::OsString;

use clap::{CommandFactory, Parser};
use tracing::error;

mod commands;
mod context;
mod pid_guard;

fn print_banner() {
    println!(
        r#"
                     ▄██████▄
                  ▄██████████▄
                 ████  ██  ████
               ▄████████████████
              ██████ ▄▄▄▄▄ █████
               ██████ ▀▀▀ ████▀
             ▀███████████████▀
               ▀▀█████████▀▀
"#
    );
}

/// Create or resume an agent task overlay.
#[derive(Parser)]
#[command(name = "phantom", about = "Create or resume an agent task overlay")]
struct TaskWrapper {
    #[command(flatten)]
    args: commands::task::TaskArgs,
}

#[derive(Parser)]
#[command(
    name = "phantom",
    version,
    about = "Semantic version control for agentic AI development"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Initialize Phantom in an existing git repository
    Init,
    /// Submit an agent's work: merge to trunk and ripple to other agents
    #[command(visible_alias = "sub")]
    Submit(commands::submit::SubmitArgs),
    /// Show status of overlays and changesets
    #[command(visible_alias = "st")]
    Status(commands::status::StatusArgs),
    /// Decompose a feature into parallel agent tasks
    Plan(commands::plan::PlanArgs),
    /// Auto-resolve merge conflicts by launching an AI agent
    #[command(visible_alias = "res")]
    Resolve(commands::resolve::ResolveArgs),
    /// Roll back a changeset and replay downstream
    #[command(visible_alias = "rb")]
    Rollback(commands::rollback::RollbackArgs),
    /// Query the event log
    #[command(visible_alias = "l")]
    Log(commands::log::LogArgs),
    /// Show materializations, or submits for a specific agent
    #[command(visible_alias = "c")]
    Changes(commands::changes::ChangesArgs),
    /// Destroy an agent's overlay
    #[command(visible_alias = "rm")]
    Destroy(commands::destroy::DestroyArgs),
    /// Watch background agents in real-time
    #[command(visible_alias = "b")]
    Background(commands::background::BackgroundArgs),
    /// Tear down Phantom: unmount all FUSE overlays and remove .phantom/
    Down(commands::down::DownArgs),

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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let result = match cli.command {
        None => {
            print_banner();
            let mut cmd = Cli::command();
            cmd.print_help().ok();
            println!();
            Ok(())
        }
        Some(Commands::Init) => commands::init::run().await,
        Some(Commands::Plan(args)) => commands::plan::run(args).await,
        Some(Commands::Submit(args)) => commands::submit::run(args).await,
        Some(Commands::Status(args)) => commands::status::run(args).await,
        Some(Commands::Resolve(args)) => commands::resolve::run(args).await,
        Some(Commands::Rollback(args)) => commands::rollback::run(args).await,
        Some(Commands::Log(args)) => commands::log::run(args).await,
        Some(Commands::Changes(args)) => commands::changes::run(args).await,
        Some(Commands::Destroy(args)) => commands::destroy::run(args).await,
        Some(Commands::Background(args)) => commands::background::run(args).await,
        Some(Commands::Down(args)) => commands::down::run(&args),
        Some(Commands::FuseMount(args)) => commands::fuse_mount::run(args),
        Some(Commands::AgentMonitor(args)) => commands::agent_monitor::run(args).await,
        Some(Commands::ExternalTask(ext_args)) => {
            // Parse external subcommand args as TaskArgs.
            // ext_args[0] is the agent name, rest are flags like --background.
            let mut argv: Vec<OsString> = vec!["phantom".into()];
            argv.extend(ext_args);
            match TaskWrapper::try_parse_from(argv) {
                Ok(w) => commands::task::run(w.args).await,
                Err(e) => {
                    // Let clap handle --help and --version display cleanly.
                    e.exit();
                }
            }
        }
    };

    if let Err(e) = result {
        error!("{:#}", e);
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
