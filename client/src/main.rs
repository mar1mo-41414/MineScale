mod cli;
mod coord;
mod crypto;
mod host;
mod join;
mod lan;
mod stun;
mod tunnel;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("mc_share=info".parse()?),
        )
        .without_time()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Host(args) => host::run(args).await,
        Commands::Join(args) => join::run(args).await,
    }
}
