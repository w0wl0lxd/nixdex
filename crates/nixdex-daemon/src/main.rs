//! Optional background daemon for nixdex.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

/// Periodically refresh the nixdex prebuilt index and serve HTTP lookups.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct Args {
    /// Release URL pattern for nix-index-database.
    #[arg(
        long,
        default_value = "https://github.com/nix-community/nix-index-database/releases/download"
    )]
    release_url: String,

    /// Architecture identifier (e.g., x86_64-linux).
    #[arg(long, default_value = "x86_64-linux")]
    architecture: String,

    /// Use the -small variant of the prebuilt index.
    #[arg(long)]
    small: bool,

    /// Cache directory for prebuilt indexes.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Refresh interval in seconds.
    #[arg(long, default_value = "3600")]
    interval: u64,

    /// HTTP server listen address.
    #[arg(long, default_value = "127.0.0.1:3750")]
    http_addr: String,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let cache_dir = args
        .cache_dir
        .unwrap_or_else(|| nixdex_core::nixdex_dir().join("prebuilt"));

    let prebuilt_config = nixdex_core::prebuilt::PrebuiltConfig {
        release_url: args.release_url,
        architecture: args.architecture,
        small: args.small,
        cache_dir,
        refresh_interval: Duration::from_secs(args.interval),
    };

    let config = nixdex_core::daemon::DaemonConfig {
        prebuilt: prebuilt_config,
        http_addr: args.http_addr,
    };

    match nixdex_core::daemon::run(&config).await {
        Ok(()) => Ok(()),
        Err(err) => Err(err).wrap_err("nixdex-daemon failed"),
    }
}
