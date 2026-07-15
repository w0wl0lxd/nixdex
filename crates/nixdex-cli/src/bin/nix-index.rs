//! Tool for generating a nixdex database.

use std::path::PathBuf;
use std::sync::OnceLock;

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
    #[arg(short, long = "compression", default_value = "22", value_parser = clap::value_parser!(i32).range(1..=22))]
    compression_level: i32,

    /// Show a stack trace in the case of a Nix evaluation error.
    #[arg(long)]
    show_trace: bool,

    /// Only add paths starting with PREFIX (for example `/bin/`).
    #[arg(long, default_value = "")]
    filter_prefix: String,

    /// Store and load results of the fetch phase in `paths.cache`.
    #[arg(long)]
    path_cache: bool,

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
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let options = nixdex_core::index::UpdateOptions {
        jobs: args.jobs,
        database: args.database,
        nixpkgs: args.nixpkgs,
        system: args.system,
        compression_level: args.compression_level,
        show_trace: args.show_trace,
        filter_prefix: args.filter_prefix,
        path_cache: args.path_cache,
        extra_scopes: args.extra_scopes,
    };

    nixdex_core::update_index(&options)
        .await
        .wrap_err("nix-index failed")?;
    Ok(())
}
