//! Tool for generating a nixdex database.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

/// Resolve the default cache directory for the nixdex database.
///
/// The result is cached as a leaked `&'static str` so it can be used as a
/// `clap` default value without heap churn on each `--help` invocation.
fn cache_dir() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let base = match std::env::var_os("XDG_CACHE_HOME") {
                Some(xdg) => PathBuf::from(xdg),
                None => match std::env::var_os("HOME") {
                    Some(home) => PathBuf::from(home).join(".cache"),
                    None => PathBuf::from("/tmp"),
                },
            };
            let path = base.join("nix-index");
            match path.into_os_string().into_string() {
                Ok(s) => s,
                Err(_) => String::from("/tmp/nix-index"),
            }
        })
        .as_str()
}

/// Builds an index for `nix-locate`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct Args {
    /// Make REQUESTS HTTP requests in parallel.
    #[arg(short = 'r', long = "requests", default_value = "100")]
    jobs: usize,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = cache_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Path to nixpkgs for which to build the index, as accepted by `nix-env -f`.
    #[arg(short = 'f', long, default_value = "<nixpkgs>")]
    nixpkgs: String,

    /// Specify system platform for which to build the index.
    #[arg(short = 's', long, value_name = "platform")]
    system: Option<String>,

    /// Zstandard compression level (1–22).
    #[arg(short, long = "compression", default_value = "19", value_parser = clap::value_parser!(i32).range(1..=22))]
    compression_level: i32,

    /// On-disk database format version (1 or 2).
    #[arg(long, default_value = "2", value_parser = clap::value_parser!(u64).range(1..=2))]
    format_version: u64,

    /// Show a stack trace in the case of a Nix evaluation error.
    #[arg(long)]
    show_trace: bool,

    /// Only add paths starting with PREFIX (for example `/bin/`).
    #[arg(long, default_value = "")]
    filter_prefix: String,

    /// Store and load results of the fetch phase in `paths.cache`.
    #[arg(long)]
    path_cache: bool,

    /// Ignore the existing `paths.cache` and re-fetch all store paths.
    #[arg(long)]
    force: bool,

    /// Cache-key used to identify a `paths.cache` file; defaults to `nixpkgs`.
    #[arg(long)]
    cache_key: Option<String>,

    /// Path to the `paths.cache` file; defaults to `<database>/paths.cache`.
    #[arg(long)]
    path_cache_file: Option<PathBuf>,

    /// Time-to-live for cache entries in seconds (0 = no expiry); defaults to 7 days.
    #[arg(long)]
    path_cache_ttl: Option<u64>,

    /// Do not synthesize `/bin/<mainProgram>` listings from `meta.mainProgram`.
    #[arg(long)]
    no_main_program: bool,

    /// Additional nixpkgs scopes to include.
    #[arg(
        long,
        default_values_t = [
            String::from("haskellPackages"),
            String::from("rPackages"),
            String::from("coqPackages"),
            String::from("texlive.pkgs"),
        ]
    )]
    extra_scopes: Vec<String>,

    /// Download a prebuilt `files` database instead of evaluating nixpkgs.
    #[arg(long)]
    download_prebuilt: bool,

    /// Base URL for the prebuilt index release assets.
    #[arg(
        long,
        default_value = "https://github.com/nix-community/nix-index-database/releases/latest/download"
    )]
    prebuilt_url: String,

    /// Architecture identifier for the prebuilt index (e.g. `x86_64-linux`).
    #[arg(long)]
    prebuilt_arch: Option<String>,

    /// Download the `-small` prebuilt variant.
    #[arg(long)]
    prebuilt_small: bool,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    if args.download_prebuilt {
        let config = nixdex_core::prebuilt::PrebuiltConfig {
            release_url: args.prebuilt_url,
            architecture: args
                .prebuilt_arch
                .unwrap_or_else(nixdex_core::prebuilt::default_architecture),
            small: args.prebuilt_small,
            cache_dir: args.database.clone(),
            refresh_interval: Duration::ZERO,
        };
        let dest = args.database.join("files");
        nixdex_core::prebuilt::download_to(&config, &dest)
            .await
            .wrap_err("failed to download prebuilt index")?;
        return Ok(());
    }

    let options = nixdex_core::index::UpdateOptions {
        jobs: args.jobs,
        database: args.database,
        nixpkgs: args.nixpkgs,
        system: args.system,
        compression_level: args.compression_level,
        format_version: args.format_version,
        show_trace: args.show_trace,
        filter_prefix: args.filter_prefix,
        path_cache: args.path_cache,
        force: args.force,
        cache_key: args.cache_key,
        path_cache_file: args.path_cache_file,
        path_cache_ttl: args.path_cache_ttl,
        main_program: !args.no_main_program,
        extra_scopes: args.extra_scopes,
    };

    nixdex_core::update_index(&options)
        .await
        .wrap_err("nix-index failed")?;
    Ok(())
}
