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

    /// Store and load results of the fetch phase in `paths.cache`.
    #[arg(long)]
    path_cache: bool,

    /// Ignore the existing `paths.cache` and re-fetch all store paths.
    #[arg(long)]
    force: bool,

    /// Cache-key used to identify a `paths.cache` file; defaults to `nixpkgs`.
    #[arg(long)]
    cache_key: Option<String>,

    /// Do not synthesize `/bin/<mainProgram>` listings from `meta.mainProgram`.
    #[arg(long)]
    no_main_program: bool,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let update_options = nixdex_core::index::UpdateOptions {
        database: args.database,
        path_cache: args.path_cache,
        force: args.force,
        cache_key: args.cache_key,
        main_program: !args.no_main_program,
        ..nixdex_core::index::UpdateOptions::default()
    };

    let config = nixdex_core::daemon::DaemonConfig {
        update_options,
        interval: Duration::from_secs(args.interval),
    };

    match nixdex_core::daemon::run(&config).await {
        Ok(()) => Ok(()),
        Err(err) => Err(err).wrap_err("nixdex-daemon failed"),
    }
}
