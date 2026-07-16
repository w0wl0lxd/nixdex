//! Logic for the `nix-locate` / `nixdex locate` command.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::OnceLock;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

use nixdex_core::database::{SearchMode, SearchOptions};
use nixdex_core::{ALL_FILE_TYPES, FileType};

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
pub enum Color {
    Always,
    Never,
    Auto,
}

/// Quickly finds the derivation providing a certain file.
#[derive(Debug, Parser)]
#[command(name = "nix-locate", author, about, version)]
pub struct Opts {
    /// Pattern for which to search.
    #[arg(value_name = "PATTERN")]
    pub pattern: String,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = default_db_dir(), env = "NIX_INDEX_DATABASE")]
    pub database: PathBuf,

    /// Treat PATTERN as regex instead of literal text. Also applies to NAME.
    #[arg(short, long)]
    pub regex: bool,

    /// Only print matches from packages whose name matches PACKAGE.
    #[arg(short, long)]
    pub package: Option<String>,

    /// Only print matches from the package that has the given HASH.
    #[arg(long, name = "HASH")]
    pub hash: Option<String>,

    /// Print all matches, not only from packages that show up in `nix-env -qa`.
    #[arg(long)]
    pub all: bool,

    /// Only print matches for files that have this type.
    ///
    /// Options: `(r)egular`, `e(x)ecutable`, `(d)irectory`, `(s)ymlink`.
    #[arg(short, long = "type", value_parser = clap::value_parser!(FileType))]
    pub r#type: Option<Vec<FileType>>,

    /// Disable grouping of paths that share the same matching component.
    #[arg(long)]
    pub no_group: bool,

    /// Whether to use colors in output.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: Color,

    /// Only print matches whose basename matches PATTERN exactly.
    #[arg(short = 'w', long)]
    pub whole_name: bool,

    /// Treat PATTERN as an absolute file path starting at the package root.
    #[arg(long)]
    pub at_root: bool,

    /// Only print attribute names of found files or directories.
    #[arg(long)]
    pub minimal: bool,
}

/// Processed form of the CLI options ready for the core search API.
struct ProcessedArgs {
    database: PathBuf,
    pattern: String,
    hash: Option<String>,
    package_pattern: Option<String>,
    exact_basename: Option<String>,
    file_type: Vec<FileType>,
    mode: SearchMode,
}

fn process_args(matches: Opts) -> ProcessedArgs {
    let start_anchor = if matches.at_root { "^" } else { "" };
    let end_anchor = if matches.whole_name { "$" } else { "" };
    let as_regex = matches.regex;

    let exact_basename = if !matches.regex && matches.whole_name && !matches.pattern.is_empty() {
        // The FST is an exact-basename index. It is only safe to use when the
        // whole-name pattern is anchored to a final path component (contains a
        // '/'); otherwise the regex `ls$` would also match basenames like
        // `als` and `xls`, which the FST lookup `ls` would omit.
        let base = nixdex_core::basename_index::basename_of(matches.pattern.as_bytes());
        if matches.pattern.contains('/') && !base.is_empty() {
            Some(String::from_utf8_lossy(base).into_owned())
        } else {
            None
        }
    } else {
        None
    };

    let make_pattern = |s: &str, wrap: bool| {
        let body = if as_regex {
            s.to_string()
        } else {
            regex::escape(s)
        };
        if wrap {
            format!("{start_anchor}{body}{end_anchor}")
        } else {
            body
        }
    };

    let pattern = make_pattern(&matches.pattern, true);
    let package_pattern = matches.package.as_deref().map(|p| make_pattern(p, false));

    let color = match matches.color {
        Color::Auto => std::io::stdout().is_terminal(),
        Color::Always => true,
        Color::Never => false,
    };

    let file_type = match matches.r#type {
        Some(types) => types,
        None => ALL_FILE_TYPES.to_vec(),
    };

    let mode = if matches.minimal {
        SearchMode::Minimal
    } else {
        SearchMode::Full {
            color,
            group: !matches.no_group,
            only_toplevel: !matches.all,
        }
    };

    ProcessedArgs {
        database: matches.database,
        pattern,
        hash: matches.hash,
        package_pattern,
        exact_basename,
        file_type,
        mode,
    }
}

/// Run a file lookup against the nixdex database.
pub fn run(matches: Opts) -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = process_args(matches);

    let options = SearchOptions {
        database: args.database,
        pattern: args.pattern,
        hash: args.hash,
        package_pattern: args.package_pattern,
        exact_basename: args.exact_basename,
        file_type: &args.file_type,
        mode: args.mode,
    };

    nixdex_core::search_database(&options).wrap_err("nix-locate failed")?;
    Ok(())
}
