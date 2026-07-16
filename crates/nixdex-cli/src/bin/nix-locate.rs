//! Tool for searching for files in nixpkgs packages.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::OnceLock;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

use nixdex_core::database::{SearchMode, SearchOptions};
use nixdex_core::{ALL_FILE_TYPES, FileType};

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

/// Escape a literal string for safe use inside a regex.
fn regex_escape(s: &str) -> String {
    const METACHARACTERS: &str = r"\^.$|?*+()[]{}";
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if METACHARACTERS.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Color policy for terminal output.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Color {
    Always,
    Never,
    Auto,
}

/// Quickly finds the derivation providing a certain file.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct Opts {
    /// Pattern for which to search.
    #[arg(value_name = "PATTERN")]
    pattern: String,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = cache_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Treat PATTERN as regex instead of literal text. Also applies to NAME.
    #[arg(short, long)]
    regex: bool,

    /// Only print matches from packages whose name matches PACKAGE.
    #[arg(short, long)]
    package: Option<String>,

    /// Only print matches from the package that has the given HASH.
    #[arg(long, name = "HASH")]
    hash: Option<String>,

    /// Print all matches, not only from packages that show up in `nix-env -qa`.
    #[arg(long)]
    all: bool,

    /// Only print matches for files that have this type.
    ///
    /// Options: `(r)egular`, `e(x)ecutable`, `(d)irectory`, `(s)ymlink`.
    #[arg(short, long = "type", value_parser = clap::value_parser!(FileType))]
    r#type: Option<Vec<FileType>>,

    /// Disable grouping of paths that share the same matching component.
    #[arg(long)]
    no_group: bool,

    /// Whether to use colors in output.
    #[arg(long, value_enum, default_value = "auto")]
    color: Color,

    /// Only print matches whose basename matches PATTERN exactly.
    #[arg(short = 'w', long)]
    whole_name: bool,

    /// Treat PATTERN as an absolute file path starting at the package root.
    #[arg(long)]
    at_root: bool,

    /// Only print attribute names of found files or directories.
    #[arg(long)]
    minimal: bool,
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
        let base = nixdex_core::basename_index::basename_of(matches.pattern.as_bytes());
        Some(String::from_utf8_lossy(base).into_owned())
    } else {
        None
    };

    let make_pattern = |s: &str, wrap: bool| {
        let body = if as_regex {
            s.to_string()
        } else {
            regex_escape(s)
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

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let matches = Opts::parse();
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
