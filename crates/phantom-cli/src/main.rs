use clap::{CommandFactory, Parser};
use tracing::error;

mod commands;
mod context;






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
    /// Assign a task to a new agent overlay
    #[command(visible_alias = "t")]
    Task(commands::task::TaskArgs),
    /// Submit an agent's work as a changeset
    #[command(visible_alias = "sub")]
    Submit(commands::submit::SubmitArgs),
    /// Show status of overlays and changesets
    #[command(visible_alias = "st")]
    Status(commands::status::StatusArgs),
    /// Materialize a changeset to trunk
    #[command(visible_alias = "mat")]
    Materialize(commands::materialize::MaterializeArgs),
    /// Roll back a changeset and replay downstream
    #[command(visible_alias = "rb")]
    Rollback(commands::rollback::RollbackArgs),
    /// Query the event log
    #[command(visible_alias = "l")]
    Log(commands::log::LogArgs),
    /// Show recent submits and materializations
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
        Some(Commands::Task(args)) => commands::task::run(args).await,
        Some(Commands::Submit(args)) => commands::submit::run(args).await,
        Some(Commands::Status(args)) => commands::status::run(args).await,
        Some(Commands::Materialize(args)) => commands::materialize::run(args).await,
        Some(Commands::Rollback(args)) => commands::rollback::run(args).await,
        Some(Commands::Log(args)) => commands::log::run(args).await,
        Some(Commands::Changes(args)) => commands::changes::run(args).await,
        Some(Commands::Destroy(args)) => commands::destroy::run(args).await,
        Some(Commands::Background(args)) => commands::background::run(args).await,
        Some(Commands::Down(args)) => commands::down::run(args).await,
        Some(Commands::FuseMount(args)) => commands::fuse_mount::run(args),
        Some(Commands::AgentMonitor(args)) => commands::agent_monitor::run(args).await,
    };

    if let Err(e) = result {
        error!("{:#}", e);
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}
