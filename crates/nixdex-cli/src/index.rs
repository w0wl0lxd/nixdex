//! Logic for the `nix-index` / `nixdex index` command.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

/// Maximum number of concurrent HTTP requests the indexer may make.
const MAX_JOBS: usize = 1000;

/// Parse and validate the `--requests` value.
fn parse_jobs(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid integer"))?;
    if n == 0 || n > MAX_JOBS {
        return Err(format!(
            "--requests must be between 1 and {MAX_JOBS}, got {n}"
        ));
    }
    Ok(n)
}

/// Resolve the default nixdex database directory.
fn default_db_dir() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            nixdex_core::nixdex_dir()
                .into_os_string()
                .into_string()
                .unwrap_or_else(|_| String::from("/tmp/nixdex"))
        })
        .as_str()
}

/// Builds an index for `nix-locate`.
#[derive(Debug, Parser)]
#[command(name = "nix-index", author, about, version)]
pub struct Args {
    /// Make REQUESTS HTTP requests in parallel.
    #[arg(short = 'r', long = "requests", default_value = "100", value_parser = parse_jobs)]
    pub jobs: usize,

    /// HTTP request timeout in seconds.
    #[arg(long, default_value = "30", value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,

    /// Number of retries for transient HTTP failures.
    #[arg(long, default_value = "4", value_parser = clap::value_parser!(u32).range(0..=20))]
    pub retries: u32,

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
    ///
    /// The expression must be a function accepting the package set, for example
    /// `p: { inherit (p) hello coreutils; }`.
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
    #[arg(short, long = "compression", default_value = "22", value_parser = clap::value_parser!(i32).range(1..=22))]
    pub compression_level: i32,

    /// On-disk database format version (1 or 2).
    ///
    /// nixdex writes format v2 by default, which is a nixdex extension.
    /// v1 is fully compatible with upstream nix-index.
    #[arg(long, default_value = "2", value_parser = clap::value_parser!(u64).range(1..=2))]
    pub format_version: u64,

    /// Show a stack trace in the case of a Nix evaluation error.
    #[arg(long)]
    pub show_trace: bool,

    /// Only add paths starting with PREFIX (for example `/bin/`).
    #[arg(long, default_value = "")]
    pub filter_prefix: String,

    /// Skip paths starting with PREFIX (can be given multiple times).
    #[arg(long)]
    pub exclude_prefix: Vec<String>,

    /// Disable nixpkgs overlays when evaluating the package set.
    ///
    /// This is equivalent to passing `--arg overlays '[]'` to `nix-env` and
    /// avoids evaluating packages added or modified by user overlays, which are
    /// unlikely to be cached on the official binary cache.
    #[arg(long)]
    pub no_overlays: bool,

    /// Allow unfree packages during nixpkgs evaluation.
    ///
    /// This sets `config.allowUnfree = true` so packages such as CUDA
    /// tools and unfree firmware are included in the index.
    #[arg(long)]
    pub allow_unfree: bool,

    /// Do not recurse into runtime references when fetching `.ls` listings.
    ///
    /// This indexes only the store paths that belong directly to each package
    /// and is much faster when full closure listings are not needed.
    #[arg(long)]
    pub no_closure: bool,

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

    /// Base URL of the Nix binary cache to fetch listings from.
    #[arg(long, default_value_t = String::from(nixdex_core::CACHE_URL))]
    pub cache_url: String,

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

    // On darwin targets, also walk the `darwin` scope to include darwin-specific
    // command packages (e.g. darwin.traceroute, darwin.xcrun).
    let mut extra_scopes = args.extra_scopes;
    let target_system = args
        .system
        .clone()
        .unwrap_or_else(nixdex_core::prebuilt::default_architecture);
    if target_system.contains("darwin") && !extra_scopes.iter().any(|s| s == "darwin") {
        extra_scopes.push(String::from("darwin"));
    }

    let options = nixdex_core::index::UpdateOptions {
        jobs: args.jobs,
        timeout: args.timeout,
        retries: args.retries,
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
        no_overlays: args.no_overlays,
        allow_unfree: args.allow_unfree,
        no_closure: args.no_closure,
        extra_scopes,
        only_eval: args.only_eval,
        cache_url: args.cache_url,
        exclude_prefix: args.exclude_prefix,
    };

    nixdex_core::update_index(&options)
        .await
        .wrap_err("nix-index failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jobs_accepts_valid_values() {
        assert_eq!(parse_jobs("1").unwrap(), 1);
        assert_eq!(parse_jobs("100").unwrap(), 100);
        assert_eq!(parse_jobs("1000").unwrap(), 1000);
    }

    #[test]
    fn parse_jobs_rejects_zero_and_huge_values() {
        assert!(parse_jobs("0").is_err());
        assert!(parse_jobs("1001").is_err());
        assert!(parse_jobs("not-a-number").is_err());
    }

    #[test]
    fn args_parsing_rejects_invalid_requests() {
        let result =
            Args::try_parse_from(["nix-index", "--requests", "0", "-d", "/tmp/nix-index-test"]);
        assert!(result.is_err());
    }
}
