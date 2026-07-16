//! Multi-purpose `nixdex` tool — currently provides package search by attribute
//! and description from the `packages.json` sidecar.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::OnceLock;

use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

use nixdex_cli::{index, locate};
use nixdex_core::package_search::{SearchDb, SearchField};

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

/// The nixdex multi-tool.
#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Search package attributes and descriptions.
    Search(SearchOpts),
    /// Show metadata for a single attribute.
    Info(InfoOpts),
    /// Generate shell completions.
    Completions(CompletionsOpts),
    /// Build a nixdex database (alias for `nix-index`).
    Index(index::Args),
    /// Find files in nixpkgs packages (alias for `nix-locate`).
    Locate(locate::Opts),
    /// Run the background daemon (alias for `nixdex-daemon`).
    Daemon(DaemonOpts),
}

/// Search package attributes and descriptions.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct SearchOpts {
    /// Pattern for which to search.
    #[arg(value_name = "PATTERN")]
    pattern: String,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = default_db_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Treat PATTERN as regex instead of literal text.
    #[arg(short, long)]
    regex: bool,

    /// Match PATTERN with case sensitivity.
    #[arg(long)]
    case_sensitive: bool,

    /// Match the whole field instead of a substring.
    #[arg(long)]
    exact: bool,

    /// Use fuzzy matching with relevance scoring (skim v2 algorithm).
    ///
    /// This ranks results by how well the pattern matches the selected field(s)
    /// and cannot be combined with `--regex` or `--exact`.
    #[arg(long, conflicts_with_all = ["regex", "exact"])]
    fuzzy: bool,

    /// Which fields to search: `attr`, `description`, `main-program`, or `both`.
    #[arg(short, long, default_value = "both")]
    field: SearchField,

    /// Only print attribute paths.
    #[arg(long)]
    name_only: bool,

    /// Maximum number of results to print.
    #[arg(short, long)]
    limit: Option<usize>,

    /// Print the number of matches instead of the matches themselves.
    #[arg(long)]
    count: bool,

    /// Print results as NDJSON instead of the default tabular format.
    #[arg(long)]
    json: bool,

    /// Whether to use colors in output.
    #[arg(long, value_enum, default_value = "auto")]
    color: Color,
}

/// Show metadata for a single package attribute.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct InfoOpts {
    /// Attribute path to look up.
    #[arg(value_name = "ATTR")]
    attr: String,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = default_db_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
}

/// Generate shell completions for `nixdex`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct CompletionsOpts {
    /// Shell for which to generate completions.
    #[arg(value_enum)]
    shell: Shell,
}

/// Options for running the background daemon.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct DaemonOpts {
    /// Release URL pattern for nix-index-database.
    #[arg(
        long,
        default_value = "https://github.com/nix-community/nix-index-database/releases/latest/download"
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

fn run_search(opts: SearchOpts) -> color_eyre::Result<()> {
    let sidecar = opts.database.join("packages.json");
    if !sidecar.exists() {
        color_eyre::eyre::bail!(
            "no package metadata sidecar found at {}. Run `nix-index` first.",
            sidecar.display()
        );
    }

    let db = SearchDb::open(&sidecar).wrap_err("failed to load package metadata sidecar")?;
    let matches = if opts.fuzzy {
        db.search_fuzzy(&opts.pattern, opts.field, opts.case_sensitive, opts.limit)
    } else {
        db.search(
            &opts.pattern,
            opts.regex,
            opts.field,
            opts.case_sensitive,
            opts.exact,
            opts.limit,
        )
    }
    .wrap_err("search failed")?;

    if opts.count {
        println!("{}", matches.len());
        return Ok(());
    }

    if opts.json {
        for record in matches {
            let line = sonic_rs::to_string(record).wrap_err("failed to serialize search result")?;
            println!("{line}");
        }
        return Ok(());
    }

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

fn run_info(opts: InfoOpts) -> color_eyre::Result<()> {
    let sidecar = opts.database.join("packages.json");
    if !sidecar.exists() {
        color_eyre::eyre::bail!(
            "no package metadata sidecar found at {}. Run `nix-index` first.",
            sidecar.display()
        );
    }

    let db = SearchDb::open(&sidecar).wrap_err("failed to load package metadata sidecar")?;
    let matches = db
        .search(&opts.attr, false, SearchField::Attr, true, true, None)
        .wrap_err("lookup failed")?;

    let Some(record) = matches.first() else {
        color_eyre::eyre::bail!("no package found with attr {}", opts.attr);
    };

    if opts.json {
        println!(
            "{}",
            sonic_rs::to_string(record).wrap_err("failed to serialize package info")?
        );
    } else {
        let desc = record.description.as_deref().map_or("—", |d| d);
        println!("{}\t{}\t{}", record.attr, record.name, desc);
    }

    Ok(())
}

fn run_completions(opts: CompletionsOpts) {
    let mut cmd = Opts::command();
    let name = cmd.get_name().to_string();
    generate(opts.shell, &mut cmd, name, &mut std::io::stdout());
}

async fn run_daemon(opts: DaemonOpts) -> color_eyre::Result<()> {
    let mut args: Vec<String> = Vec::new();
    if let Some(cache_dir) = opts.cache_dir {
        args.push("--cache-dir".into());
        args.push(cache_dir.to_string_lossy().into_owned());
    }
    args.extend([
        "--release-url".into(),
        opts.release_url,
        "--architecture".into(),
        opts.architecture,
        "--interval".into(),
        opts.interval.to_string(),
        "--http-addr".into(),
        opts.http_addr,
    ]);
    if opts.small {
        args.push("--small".into());
    }

    let mut child = tokio::process::Command::new("nixdex-daemon")
        .args(args)
        .spawn()
        .wrap_err("failed to spawn `nixdex-daemon`; is it installed and on PATH?")?;

    let status = child
        .wait()
        .await
        .wrap_err("failed to wait for `nixdex-daemon`")?;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }

    Ok(())
}

fn colored(text: &str, code: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

#[derive(Debug, Parser)]
#[command(name = "nixdex", author, about, version)]
struct Opts {
    #[command(subcommand)]
    cmd: Cmd,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let opts = Opts::parse();

    match opts.cmd {
        Cmd::Search(search_opts) => run_search(search_opts),
        Cmd::Info(info_opts) => run_info(info_opts),
        Cmd::Completions(opts) => {
            run_completions(opts);
            Ok(())
        }
        Cmd::Index(index_opts) => index::run(index_opts).await,
        Cmd::Locate(locate_opts) => locate::run(locate_opts),
        Cmd::Daemon(daemon_opts) => run_daemon(daemon_opts).await,
    }
}
