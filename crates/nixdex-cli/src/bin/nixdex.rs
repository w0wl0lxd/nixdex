//! Multi-purpose `nixdex` tool — currently provides package search by attribute
//! and description from the `packages.json` sidecar.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::OnceLock;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

use nixdex_core::package_search::{SearchDb, SearchField};

/// Resolve the default cache directory for the nixdex database.
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

/// Color policy for terminal output.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Color {
    Always,
    Never,
    Auto,
}

impl Color {
    fn use_color(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => std::io::stdout().is_terminal(),
        }
    }
}

/// Search nixpkgs package metadata by attribute or description.
#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Search package attributes and descriptions.
    Search(SearchOpts),
}

/// Search package attributes and descriptions.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct SearchOpts {
    /// Pattern for which to search.
    #[arg(value_name = "PATTERN")]
    pattern: String,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = cache_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Treat PATTERN as regex instead of literal text.
    #[arg(short, long)]
    regex: bool,

    /// Which fields to search: `attr`, `description`, or `both`.
    #[arg(short, long, default_value = "both")]
    field: SearchField,

    /// Only print attribute paths.
    #[arg(long)]
    name_only: bool,

    /// Maximum number of results to print.
    #[arg(short, long)]
    limit: Option<usize>,

    /// Whether to use colors in output.
    #[arg(long, value_enum, default_value = "auto")]
    color: Color,
}

fn run_search(opts: SearchOpts) -> color_eyre::Result<()> {
    let sidecar = opts.database.join("packages.json");
    if !sidecar.exists() {
        color_eyre::eyre::bail!(
            "no package metadata sidecar found at {}. Run `nix-index` first.",
            sidecar.display()
        );
    }

    let db = SearchDb::open(&sidecar).wrap_err("failed to load package metadata sidecar")?;
    let matches = db
        .search(&opts.pattern, opts.regex, opts.field, opts.limit)
        .wrap_err("search failed")?;

    let use_color = opts.color.use_color();
    for record in matches {
        let desc = record.description.as_deref().map_or("—", |d| d);
        if opts.name_only {
            println!("{}", record.attr);
        } else if use_color {
            println!(
                "{}\t{}\t{}",
                colored(record.attr.as_str(), "1;32"),
                colored(record.name.as_str(), "1"),
                desc
            );
        } else {
            println!("{}\t{}\t{}", record.attr, record.name, desc);
        }
    }

    Ok(())
}

fn colored(text: &str, code: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

#[derive(Debug, Parser)]
#[command(author, about, version)]
struct Opts {
    #[command(subcommand)]
    cmd: Cmd,
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let opts = Opts::parse();

    match opts.cmd {
        Cmd::Search(search_opts) => run_search(search_opts),
    }
}
