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
    #[command(visible_alias = "d")]
    Dispatch(commands::dispatch::DispatchArgs),
    /// Submit an agent's work as a changeset
    #[command(visible_alias = "sub")]
    Submit(commands::submit::SubmitArgs),
    /// Show status of overlays and changesets
    #[command(visible_alias = "st")]
    Status,
    /// Materialize a changeset to trunk
    #[command(visible_alias = "mat")]
    Materialize(commands::materialize::MaterializeArgs),
    /// Roll back a changeset and replay downstream
    #[command(visible_alias = "rb")]
    Rollback(commands::rollback::RollbackArgs),
    /// Query the event log
    #[command(visible_alias = "l")]
    Log(commands::log::LogArgs),
    /// Destroy an agent's overlay
    #[command(visible_alias = "rm")]
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
