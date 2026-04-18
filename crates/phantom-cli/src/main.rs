// PID guard, FUSE mount, and TTY detection require libc calls.
#![allow(unsafe_code)]
use std::ffi::OsString;

use clap::Parser;
use tracing::error;

mod cli;
mod commands;
mod context;
mod fs;
mod pid_guard;
mod services;
mod ui;

use cli::{Cli, Commands, TaskWrapper};

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

/// Print a styled, grouped overview of `ph` commands.
///
/// Called when `ph` is invoked with no arguments. Uses `console::style` so
/// colors and weights are automatically disabled when stdout is not a TTY
/// or `NO_COLOR` is set.
fn print_overview() {
    let bold = |s: &str| console::style(s.to_string()).bold().to_string();
    let dim = |s: &str| console::style(s.to_string()).dim().to_string();
    let cyan = |s: &str| console::style(s.to_string()).cyan().bold().to_string();
    let green = |s: &str| console::style(s.to_string()).green().bold().to_string();
    let yellow = |s: &str| console::style(s.to_string()).yellow().bold().to_string();
    let magenta = |s: &str| console::style(s.to_string()).magenta().bold().to_string();

    // Header
    println!(
        "  {}",
        bold("Semantic version control for agentic AI development")
    );
    println!();
    println!("  {}", dim("Usage:"));
    println!(
        "    {} {}     {}",
        bold("ph"),
        cyan("<agent>  "),
        dim("Create or resume an agent overlay + session"),
    );
    println!(
        "    {} {}     {}",
        bold("ph"),
        cyan("<command>"),
        dim("Run a built-in command (see below)"),
    );
    println!();

    // Group: Agents (create/inspect/resume)
    println!("  {}", green("AGENTS"));
    row("tasks", "t", "List every agent overlay with live status");
    row(
        "resume",
        "re",
        "Pick an interactive agent session and re-attach to it",
    );
    row(
        "background",
        "b",
        "Watch background agents streaming output in real time",
    );
    row("exec", "x", "Run a command inside an agent's overlay view");
    println!();

    // Group: Merge pipeline
    println!("  {}", yellow("MERGE PIPELINE"));
    row(
        "submit",
        "sub",
        "Semantic 3-way merge to trunk + ripple to other agents",
    );
    row(
        "resolve",
        "res",
        "Launch an AI agent on a conflicted changeset (experimental)",
    );
    row(
        "plan",
        "",
        "AI-decompose a feature into parallel agents (experimental)",
    );
    row(
        "rollback",
        "rb",
        "Drop a changeset, revert trunk, flag downstream agents",
    );
    println!();

    // Group: Inspection
    println!("  {}", cyan("INSPECT"));
    row("status", "st", "Overlays, changesets, and trunk state");
    row("log", "l", "Query the append-only event log with filters");
    row(
        "changes",
        "c",
        "Recent submits and materializations on trunk",
    );
    println!();

    // Group: Lifecycle
    println!("  {}", magenta("LIFECYCLE"));
    row("init", "", "Initialize Phantom in the current git repo");
    row(
        "remove",
        "rm",
        "Remove an agent's overlay and unmount FUSE (no prompt)",
    );
    row(
        "down",
        "",
        "Unmount everything and remove .phantom/ (prompts)",
    );
    println!();

    // Footer
    println!("  {}", dim("Options:"));
    println!("    {}     {}", bold("-h, --help   "), dim("Print help"));
    println!("    {}     {}", bold("-V, --version"), dim("Print version"));
    println!();
    println!(
        "  {} {}{}",
        dim("Tip: any unrecognized subcommand is treated as an agent name — try"),
        bold("ph my-agent"),
        dim("."),
    );
}

fn row(name: &str, alias: &str, description: &str) {
    // Column widths are based on *visible* characters, not byte-length of
    // ANSI-wrapped strings. Pad the plain text first, then style.
    const NAME_WIDTH: usize = 12;
    const ALIAS_WIDTH: usize = 5; // accommodates "/rb", "/sub", "/res"

    let name_pad = NAME_WIDTH.saturating_sub(name.chars().count());
    let alias_text = if alias.is_empty() {
        String::new()
    } else {
        format!("/{alias}")
    };
    let alias_pad = ALIAS_WIDTH.saturating_sub(alias_text.chars().count());

    let name_styled = console::style(name).bold();
    let alias_styled = if alias.is_empty() {
        console::style(String::new())
    } else {
        console::style(alias_text).cyan()
    };

    println!(
        "    {name_styled}{name_spaces}{alias_styled}{alias_spaces}  {desc}",
        name_spaces = " ".repeat(name_pad),
        alias_spaces = " ".repeat(alias_pad),
        desc = console::style(description).dim(),
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt::init();

    // Intercept bare `ph` and `ph --help` / `ph -h` so we can render a custom
    // styled overview instead of clap's auto-generated help. Anything beyond
    // a single help flag is left to clap (subcommand help still works).
    let raw_args: Vec<OsString> = std::env::args_os().collect();
    let only_help_flag =
        raw_args.len() == 2 && matches!(raw_args[1].to_str(), Some("--help" | "-h" | "help"));
    if raw_args.len() == 1 || only_help_flag {
        print_banner();
        print_overview();
        return;
    }

    let cli = Cli::parse();

    let result = match cli.command {
        None => {
            print_banner();
            print_overview();
            Ok(())
        }
        Some(Commands::Init) => commands::init::run().await,
        Some(Commands::Tasks(args)) => commands::tasks::run(args).await,
        Some(Commands::Plan(args)) => commands::plan::run(args).await,
        Some(Commands::Submit(args)) => commands::submit::run(args).await,
        Some(Commands::Status(args)) => commands::status::run(args).await,
        Some(Commands::Resolve(args)) => commands::resolve::run(args).await,
        Some(Commands::Rollback(args)) => commands::rollback::run(args).await,
        Some(Commands::Log(args)) => commands::log::run(args).await,
        Some(Commands::Changes(args)) => commands::changes::run(args).await,
        Some(Commands::Remove(args)) => commands::remove::run(args).await,
        Some(Commands::Background(args)) => commands::background::run(args).await,
        Some(Commands::Resume(args)) => commands::resume::run(args).await,
        Some(Commands::Down(args)) => commands::down::run(&args),
        Some(Commands::Exec(args)) => commands::exec::run(&args),
        Some(Commands::Verify(args)) => commands::verify::run(args).await,
        Some(Commands::FuseMount(args)) => commands::fuse_mount::run(args),
        Some(Commands::AgentMonitor(args)) => commands::agent_monitor::run(args).await,
        Some(Commands::ExternalTask(ext_args)) => {
            // Parse external subcommand args as TaskArgs.
            // ext_args[0] is the agent name, rest are flags like --background.
            let mut argv: Vec<OsString> = vec!["ph".into()];
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
