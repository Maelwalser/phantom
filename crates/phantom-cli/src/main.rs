// PID guard, FUSE mount, and TTY detection require libc calls.
#![allow(unsafe_code)]
use std::ffi::OsString;

use clap::{CommandFactory, Parser};
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

#[tokio::main(flavor = "current_thread")]
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
        Some(Commands::Tasks(args)) => commands::tasks::run(args).await,
        Some(Commands::Plan(args)) => commands::plan::run(args).await,
        Some(Commands::Submit(args)) => commands::submit::run(args).await,
        Some(Commands::Status(args)) => commands::status::run(args).await,
        Some(Commands::Resolve(args)) => commands::resolve::run(args).await,
        Some(Commands::Rollback(args)) => commands::rollback::run(args).await,
        Some(Commands::Log(args)) => commands::log::run(args).await,
        Some(Commands::Changes(args)) => commands::changes::run(args).await,
        Some(Commands::Destroy(args)) => commands::destroy::run(args).await,
        Some(Commands::Background(args)) => commands::background::run(args).await,
        Some(Commands::Resume(args)) => commands::resume::run(args).await,
        Some(Commands::Down(args)) => commands::down::run(&args),
        Some(Commands::Exec(args)) => commands::exec::run(&args),
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
