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

    /// Sort results: `relevance` (default), `none`, `size`/`size-asc`,
    /// `size-desc`, or `attr`/`attr-asc`.
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

    /// Disable the resident daemon and run a local search directly. The daemon
    /// keeps the index resident for warm sub-100ms queries; without it each
    /// invocation reloads the index from disk.
    #[arg(long)]
    pub no_daemon: bool,
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
    literal_pattern: Option<String>,
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
        literal_pattern: if matches.regex {
            None
        } else {
            Some(matches.pattern.clone())
        },
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
///
/// Prefers a resident daemon (auto-spawning one if none is listening) for warm
/// sub-100ms queries, and falls back to a local search on any daemon failure.
pub async fn run(matches: Opts) -> color_eyre::Result<()> {
    let _ = color_eyre::install().ok();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    if !matches.no_daemon && !crate::daemon_client::daemon_disabled() {
        match locate_via_daemon(&matches).await {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
                return Ok(());
            }
            Err(err) => {
                eprintln!("nix-locate: daemon unavailable ({err}); falling back to local search");
            }
        }
    }

    let args = process_args(matches)?;

    let options = SearchOptions {
        database: args.database,
        pattern: args.pattern,
        hash: args.hash,
        package_pattern: args.package_pattern,
        exact_basename: args.exact_basename,
        exact_path: args.exact_path,
        path_prefix: args.path_prefix,
        literal_pattern: args.literal_pattern,
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

/// Query the resident daemon, spawning one if necessary, and return rendered
/// output lines. Returns `Err` if no daemon is available or the request fails,
/// so the caller can fall back to a local search.
async fn locate_via_daemon(opts: &Opts) -> Result<Vec<String>, crate::daemon_client::DaemonError> {
    let addr = crate::daemon_client::resolve_addr();
    let client = crate::daemon_client::ensure_client(&opts.database, &addr, "resident").await;
    let query = daemon_query(opts);
    let response = client.locate(&query).await?;
    Ok(crate::daemon_client::render(
        &response,
        opts.json,
        opts.minimal,
    ))
}

/// Build the `/nix-locate` query parameters from the CLI options.
fn daemon_query(opts: &Opts) -> Vec<(String, String)> {
    let mut q: Vec<(String, String)> = vec![
        ("pattern".into(), opts.pattern.clone()),
        ("regex".into(), opts.regex.to_string()),
        ("at_root".into(), opts.at_root.to_string()),
        ("whole_name".into(), opts.whole_name.to_string()),
        ("minimal".into(), opts.minimal.to_string()),
        ("count".into(), opts.count.to_string()),
        ("exclude_fhs".into(), opts.exclude_fhs.to_string()),
    ];
    if let Some(p) = &opts.package {
        q.push(("package".into(), p.clone()));
    }
    if let Some(h) = &opts.hash {
        q.push(("hash".into(), h.clone()));
    }
    if let Some(types) = file_type_chars(opts.r#type.as_ref()) {
        q.push(("type".into(), types));
    }
    if let Some(l) = opts.limit {
        q.push(("limit".into(), l.to_string()));
    }
    if let Some(s) = &opts.sort {
        q.push(("sort".into(), s.clone()));
    }
    if let Some(m) = opts.min_size {
        q.push(("min_size".into(), m.to_string()));
    }
    if let Some(m) = opts.max_size {
        q.push(("max_size".into(), m.to_string()));
    }
    q
}

/// Map the `--type` selection to the single-character codes the daemon expects.
fn file_type_chars(types: Option<&Vec<FileType>>) -> Option<String> {
    let types = types?;
    if types.is_empty() {
        return None;
    }
    let mut out = String::new();
    for t in types {
        match t {
            FileType::Regular { executable: false } => out.push('r'),
            FileType::Regular { executable: true } => out.push('x'),
            FileType::Directory => out.push('d'),
            FileType::Symlink => out.push('s'),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_db_dir_matches_nixdex_cache_dir() {
        // The `nixdex locate` subcommand (and the library `Opts` type used by
        // it) must default to `~/.cache/nixdex`, not the upstream-compatible
        // `~/.cache/nix-index` used by the standalone `nix-locate` binary.
        assert_eq!(PathBuf::from(default_db_dir()), nixdex_core::nixdex_dir());
    }

    #[test]
    fn opts_parsing_defaults_database_to_nixdex_cache_dir() {
        let opts = Opts::try_parse_from(["nix-locate", "somepattern"]).expect("parse defaults");
        assert_eq!(opts.database, nixdex_core::nixdex_dir());
    }

    #[test]
    fn opts_parsing_explicit_db_overrides_default() {
        let opts = Opts::try_parse_from([
            "nix-locate",
            "-d",
            "/tmp/nix-locate-explicit",
            "somepattern",
        ])
        .expect("parse with explicit db");
        assert_eq!(opts.database, PathBuf::from("/tmp/nix-locate-explicit"));
    }
}
