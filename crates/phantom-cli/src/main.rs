use clap::Parser;
use tracing::error;

mod commands;
mod context;

#[derive(Parser)]
#[command(
    name = "phantom",
    version,
    about = "Semantic version control for agentic AI development"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Initialize Phantom in an existing git repository
    Up,
    /// Assign a task to a new agent overlay
    Dispatch(commands::dispatch::DispatchArgs),
    /// Submit an agent's work as a changeset
    Submit(commands::submit::SubmitArgs),
    /// Show status of overlays and changesets
    Status,
    /// Materialize a changeset to trunk
    Materialize(commands::materialize::MaterializeArgs),
    /// Roll back a changeset and replay downstream
    Rollback(commands::rollback::RollbackArgs),
    /// Query the event log
    Log(commands::log::LogArgs),
    /// Destroy an agent's overlay
    Destroy(commands::destroy::DestroyArgs),
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Up => commands::up::run().await,
        Commands::Dispatch(args) => commands::dispatch::run(args).await,
        Commands::Submit(args) => commands::submit::run(args).await,
        Commands::Status => commands::status::run().await,
        Commands::Materialize(args) => commands::materialize::run(args).await,
        Commands::Rollback(args) => commands::rollback::run(args).await,
        Commands::Log(args) => commands::log::run(args).await,
        Commands::Destroy(args) => commands::destroy::run(args).await,
    };

    if let Err(e) = result {
        error!("{:#}", e);
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}
