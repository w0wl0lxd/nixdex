//! Logic for the `nix-locate` / `nixdex locate` command.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

use clap::Parser;
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

use nixdex_core::database::{SearchMode, SearchOptions, SearchSort};
use nixdex_core::{ALL_FILE_TYPES, FileType};

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

/// Color policy for terminal output.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Color {
    Always,
    Never,
    Auto,
}

const LONG_USAGE: &str = r"How to use
==========

In the simplest case, just run `nix-locate part/of/file/path` to search for all packages that contain
a file matching that path:

    $ nix-locate 'bin/firefox'
    ...all packages containing a file named 'bin/firefox'

Before using this tool, you first need to generate a nix-index database.
Use the `nix-index` tool to do that.

Limitations
===========

* This tool can only find packages which are built by hydra, because only those packages
  will have file listings that are indexed by nix-index.

* We can't know the precise attribute path for every package, so if you see the syntax `(attr)`
  in the output, that means that `attr` is not the target package but that it
  depends (perhaps indirectly) on the package that contains the searched file. Example:

      $ nix-locate 'bin/xmonad'
      (xmonad-with-packages.out)      0 s /nix/store/nl581g5kv3m2xnmmfgb678n91d7ll4vv-ghc-8.0.2-with-packages/bin/xmonad

  This means that we don't know what nixpkgs attribute produces /nix/store/nl581g5kv3m2xnmmfgb678n91d7ll4vv-ghc-8.0.2-with-packages,
  but we know that `xmonad-with-packages.out` requires it.
";

/// Quickly finds the derivation providing a certain file.
#[derive(Debug, Parser)]
#[command(name = "nix-locate", author, about, version, after_long_help = LONG_USAGE)]
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
    /// If the option is given multiple times, a file will be printed if it has
    /// any of the given types.
    ///
    /// Options: `(r)egular file`, `e(x)ecutable`, `(d)irectory`, `(s)ymlink`.
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

    /// Output results as one JSON object per line instead of the default
    /// human-readable format.
    #[arg(long)]
    pub json: bool,

    /// Maximum number of results to print.
    #[arg(short, long)]
    pub limit: Option<usize>,

    /// Print the number of matches instead of the matches themselves.
    #[arg(long)]
    pub count: bool,

    /// Sort results: `size`, `size-asc`, `size-desc`, or `attr`/`attr-asc`.
    #[arg(long)]
    pub sort: Option<String>,

    /// Only print files with size >= MIN_SIZE bytes.
    #[arg(long)]
    pub min_size: Option<u64>,

    /// Only print files with size <= MAX_SIZE bytes.
    #[arg(long)]
    pub max_size: Option<u64>,

    /// Exclude results from FHS-style packages (`-fhs` / `-usr-target`).
    #[arg(long)]
    pub exclude_fhs: bool,
}

/// Processed form of the CLI options ready for the core search API.
struct ProcessedArgs {
    database: PathBuf,
    pattern: String,
    hash: Option<String>,
    package_pattern: Option<String>,
    exact_basename: Option<String>,
    exact_path: Option<String>,
    path_prefix: Option<String>,
    file_type: Vec<FileType>,
    mode: SearchMode,
    json: bool,
    limit: Option<usize>,
    count: bool,
    sort: nixdex_core::database::SearchSort,
    min_size: Option<u64>,
    max_size: Option<u64>,
    exclude_fhs: bool,
}

fn process_args(matches: Opts) -> color_eyre::Result<ProcessedArgs> {
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

    // Determine if we can use the path index for rooted/prefix queries
    let (exact_path, path_prefix) =
        if !matches.regex && matches.at_root && !matches.pattern.is_empty() {
            // Normalize the pattern to ensure it starts with "/" for path index lookups
            let normalized = if matches.pattern.starts_with('/') {
                matches.pattern.clone()
            } else {
                format!("/{}", matches.pattern)
            };

            if matches.whole_name {
                // Pattern is anchored at end too, so it's an exact full path
                (Some(normalized), None)
            } else {
                // Pattern may be a prefix; use prefix lookup
                (None, Some(normalized))
            }
        } else {
            (None, None)
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

    let sort = match matches.sort {
        Some(s) => SearchSort::from_str(&s)
            .map_err(|err| color_eyre::eyre::eyre!("invalid --sort value '{s}': {err}"))?,
        None => SearchSort::None,
    };

    Ok(ProcessedArgs {
        database: matches.database,
        pattern,
        hash: matches.hash,
        package_pattern,
        exact_basename,
        exact_path,
        path_prefix,
        file_type,
        mode,
        json: matches.json,
        limit: matches.limit,
        count: matches.count,
        sort,
        min_size: matches.min_size,
        max_size: matches.max_size,
        exclude_fhs: matches.exclude_fhs,
    })
}

/// Run a file lookup against the nixdex database.
pub fn run(matches: Opts) -> color_eyre::Result<()> {
    let _ = color_eyre::install().ok();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let args = process_args(matches)?;

    let options = SearchOptions {
        database: args.database,
        pattern: args.pattern,
        hash: args.hash,
        package_pattern: args.package_pattern,
        exact_basename: args.exact_basename,
        exact_path: args.exact_path,
        path_prefix: args.path_prefix,
        file_type: &args.file_type,
        mode: args.mode,
        json: args.json,
        limit: args.limit,
        count: args.count,
        sort: args.sort,
        min_size: args.min_size,
        max_size: args.max_size,
        exclude_fhs: args.exclude_fhs,
    };

    nixdex_core::search_database(&options).wrap_err("nix-locate failed")?;
    Ok(())
}
