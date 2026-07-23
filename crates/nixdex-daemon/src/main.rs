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
        default_value = "https://github.com/nix-community/nix-index-database/releases/latest/download"
    )]
    release_url: String,

    /// Architecture identifier (e.g., x86_64-linux).
    #[arg(long, default_value_t = nixdex_core::prebuilt::default_architecture())]
    architecture: String,

    /// Use the -small variant of the prebuilt index.
    #[arg(long)]
    small: bool,

    /// Maximum concurrent connections used to download the prebuilt index.
    #[arg(long, default_value_t = nixdex_core::prebuilt::DEFAULT_MAX_CONNECTIONS)]
    max_connections: usize,

    /// Cache directory for prebuilt indexes.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Refresh interval in seconds.
    #[arg(long, default_value = "3600", value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,

    /// HTTP server listen address.
    #[arg(long, default_value = "127.0.0.1:3750")]
    http_addr: String,

    /// Serve an existing local index directory instead of downloading a prebuilt index.
    ///
    /// The directory must contain a NIXI `files` database. Sidecar files
    /// (`files.basename.*`, `packages.json`) are loaded if present.
    #[arg(long)]
    database: Option<PathBuf>,

    /// Bearer token required for `POST /reload` when not bound to loopback.
    /// If unset, `/reload` is only accepted from loopback addresses.
    #[arg(long, env = "NIXDEX_ADMIN_TOKEN")]
    admin_token: Option<String>,

    /// Index cache mode: `resident` builds sidecar indexes eagerly at load,
    /// `lru` defers them until first needed.
    #[arg(long, value_enum, default_value_t = IndexCacheModeArg::Resident)]
    index_cache_mode: IndexCacheModeArg,
}

/// Index cache mode selected on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Default)]
enum IndexCacheModeArg {
    /// Build sidecar indexes eagerly at load.
    #[default]
    Resident,
    /// Build sidecar indexes lazily on first use.
    Lru,
}

impl From<IndexCacheModeArg> for nixdex_core::daemon::IndexCacheMode {
    fn from(value: IndexCacheModeArg) -> Self {
        match value {
            IndexCacheModeArg::Resident => Self::Resident,
            IndexCacheModeArg::Lru => Self::Lru,
        }
    }
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
        max_connections: args.max_connections,
    };

    let config = nixdex_core::daemon::DaemonConfig {
        prebuilt: prebuilt_config,
        http_addr: args.http_addr,
        local_database: args.database,
        local_refresh_interval: Duration::from_secs(args.interval),
        admin_token: args.admin_token,
        index_cache_mode: args.index_cache_mode.into(),
    };

    match nixdex_core::daemon::run(&config).await {
        Ok(()) => Ok(()),
        Err(err) => Err(err).wrap_err("nixdex-daemon failed"),
    }
}
