mod cli;
mod coord;
mod crypto;
mod diag;
mod host;
mod join;
mod lan;
mod logfile;
mod relay;
mod stun;
mod telemetry;
mod transport;
mod tunnel;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    // Console output, plus the same lines appended to a log file the moment
    // they happen (see logfile.rs for where it lives).
    let file_log = logfile::FileLog::open("mc-share", env!("CARGO_PKG_VERSION"));
    {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("mc_share=info".parse()?),
            )
            .with(tracing_subscriber::fmt::layer().without_time())
            .with(file_log.clone().map(|f| logfile::FileLayer::new(f, false)))
            .init();
    }
    if let Some(f) = &file_log {
        tracing::info!("Log file: {}", f.path().display());
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Host(args) => host::run(args).await,
        Commands::Join(args) => join::run(args).await,
    }
}
