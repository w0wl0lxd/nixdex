//! Logic for the `nix-index` / `nixdex index` command.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

/// Resolve the default nix-index database directory.
fn default_db_dir() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            nixdex_core::nix_index_dir()
                .into_os_string()
                .into_string()
                .unwrap_or_else(|_| String::from("/tmp/nix-index"))
        })
        .as_str()
}

/// Builds an index for `nix-locate`.
#[derive(Debug, Parser)]
#[command(name = "nix-index", author, about, version)]
pub struct Args {
    /// Make REQUESTS HTTP requests in parallel.
    #[arg(short = 'r', long = "requests", default_value = "100")]
    pub jobs: usize,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = default_db_dir(), env = "NIX_INDEX_DATABASE")]
    pub database: PathBuf,

    /// Path to nixpkgs for which to build the index, as accepted by `nix-env -f`.
    #[arg(short = 'f', long, default_value = "<nixpkgs>")]
    pub nixpkgs: String,

    /// Specify system platform for which to build the index.
    #[arg(short = 's', long, value_name = "platform")]
    pub system: Option<String>,

    /// Pass a Nix function to `nix-eval-jobs --select` to filter or transform the
    /// evaluation root before attribute traversal.
    #[arg(long, value_name = "EXPR")]
    pub select: Option<String>,

    /// Pass `--no-instantiate` to `nix-eval-jobs` for faster read-only evaluation.
    ///
    /// This is most useful with `--only-eval`, since it may leave output paths
    /// uninstantiated.
    #[arg(long)]
    pub no_instantiate: bool,

    /// Disable `nix-eval-jobs --check-cache-status` to avoid blocking eval
    /// workers on narinfo lookups.
    #[arg(long)]
    pub no_check_cache_status: bool,

    /// Zstandard compression level (1–22).
    #[arg(short, long = "compression", default_value = "19", value_parser = clap::value_parser!(i32).range(1..=22))]
    pub compression_level: i32,

    /// On-disk database format version (1 or 2).
    #[arg(long, default_value = "2", value_parser = clap::value_parser!(u64).range(1..=2))]
    pub format_version: u64,

    /// Show a stack trace in the case of a Nix evaluation error.
    #[arg(long)]
    pub show_trace: bool,

    /// Only add paths starting with PREFIX (for example `/bin/`).
    #[arg(long, default_value = "")]
    pub filter_prefix: String,

    /// Build a small database containing only files under `/bin/`.
    ///
    /// This is equivalent to `--filter-prefix /bin/` and is much faster to build
    /// and query for command-not-found use cases.
    #[arg(long)]
    pub small: bool,

    /// Store and load results of the fetch phase in `paths.cache`.
    #[arg(long)]
    pub path_cache: bool,

    /// Ignore the existing `paths.cache` and re-fetch all store paths.
    #[arg(long)]
    pub force: bool,

    /// Cache-key used to identify a `paths.cache` file; defaults to `nixpkgs`.
    #[arg(long)]
    pub cache_key: Option<String>,

    /// Path to the `paths.cache` file; defaults to `<database>/paths.cache`.
    #[arg(long)]
    pub path_cache_file: Option<PathBuf>,

    /// Time-to-live for cache entries in seconds (0 = no expiry); defaults to 7 days.
    #[arg(long)]
    pub path_cache_ttl: Option<u64>,

    /// Do not synthesize `/bin/<mainProgram>` listings from `meta.mainProgram`.
    #[arg(long)]
    pub no_main_program: bool,

    /// Only evaluate nixpkgs; do not fetch listings or write the files database.
    #[arg(long)]
    pub only_eval: bool,

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
    pub extra_scopes: Vec<String>,

    /// Download a prebuilt `files` database instead of evaluating nixpkgs.
    #[arg(long)]
    pub download_prebuilt: bool,

    /// Base URL for the prebuilt index release assets.
    #[arg(
        long,
        default_value = "https://github.com/nix-community/nix-index-database/releases/latest/download"
    )]
    pub prebuilt_url: String,

    /// Architecture identifier for the prebuilt index (e.g. `x86_64-linux`).
    #[arg(long)]
    pub prebuilt_arch: Option<String>,

    /// Download the `-small` prebuilt variant.
    #[arg(long)]
    pub prebuilt_small: bool,
}

/// Run the index build.
pub async fn run(args: Args) -> color_eyre::Result<()> {
    let _ = color_eyre::install().ok();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

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

    let filter_prefix = if args.small {
        if !args.filter_prefix.is_empty() && args.filter_prefix != "/bin/" {
            color_eyre::eyre::bail!(
                "--small is incompatible with --filter-prefix '{}'",
                args.filter_prefix
            );
        }
        String::from("/bin/")
    } else {
        args.filter_prefix
    };

    let options = nixdex_core::index::UpdateOptions {
        jobs: args.jobs,
        database: args.database,
        nixpkgs: args.nixpkgs,
        system: args.system,
        select: args.select,
        no_instantiate: args.no_instantiate,
        check_cache_status: !args.no_check_cache_status,
        compression_level: args.compression_level,
        format_version: args.format_version,
        show_trace: args.show_trace,
        filter_prefix,
        small: args.small,
        path_cache: args.path_cache,
        force: args.force,
        cache_key: args.cache_key,
        path_cache_file: args.path_cache_file,
        path_cache_ttl: args.path_cache_ttl,
        main_program: !args.no_main_program,
        extra_scopes: args.extra_scopes,
        only_eval: args.only_eval,
    };

    nixdex_core::update_index(&options)
        .await
        .wrap_err("nix-index failed")?;
    Ok(())
}
