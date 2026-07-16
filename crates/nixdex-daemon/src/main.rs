//! Optional background daemon for nixdex.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

/// Periodically refresh the nixdex index.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct Args {
    /// Directory where the index is stored.
    #[arg(
        short,
        long = "db",
        env = "NIX_INDEX_DATABASE",
        default_value = "/tmp/nix-index"
    )]
    database: PathBuf,

    /// Refresh interval in seconds.
    #[arg(long, default_value = "86400")]
    interval: u64,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let config = nixdex_core::daemon::DaemonConfig {
        database: args.database,
        interval: Duration::from_secs(args.interval),
    };

    match nixdex_core::daemon::run(&config).await {
        Ok(()) => Ok(()),
        Err(err) => Err(err).wrap_err("nixdex-daemon failed"),
    }
}
