//! Multi-purpose `nixdex` tool — currently provides package search by attribute
//! and description from the `packages.json` sidecar.

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use color_eyre::eyre::WrapErr;
use tracing_subscriber::EnvFilter;

use nixdex_cli::{index, locate};
use nixdex_core::package_search::{SearchDb, SearchField, SearchSort};

/// Detect which comma command is available on `$PATH` ("," or "comma").
/// Returns the detected command token, or `None` if neither is available.
#[cfg(unix)]
fn comma_available() -> Option<&'static str> {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            // Prefer "," over "comma" when both are available
            let comma_candidate = dir.join(",");
            if comma_candidate.is_file()
                && std::fs::metadata(&comma_candidate)
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            {
                return Some(",");
            }
            let comma_name_candidate = dir.join("comma");
            if comma_name_candidate.is_file()
                && std::fs::metadata(&comma_name_candidate)
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            {
                return Some("comma");
            }
            None
        })
    })
}

#[cfg(not(unix))]
fn comma_available() -> Option<&'static str> {
    None
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
    /// Print database statistics and sidecar status.
    Stats(StatsOpts),
    /// Generate shell completions.
    Completions(CompletionsOpts),
    /// Build a nixdex database (alias for `nix-index`).
    Index(index::Args),
    /// Find files in nixpkgs packages (alias for `nix-locate`).
    Locate(locate::Opts),
    /// Find the package that provides a command (similar to `which`).
    Which(WhichOpts),
    /// Download the latest prebuilt index.
    Update(UpdateOpts),
    /// Generate sidecar indexes for an existing `files` database.
    GenerateSidecars(GenerateSidecarsOpts),
    /// Print a command-not-found hint for a missing command.
    CommandNotFound(CommandNotFoundOpts),
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
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
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

    /// Which fields to search.
    #[arg(short, long, value_enum, default_value_t = SearchField::Both)]
    field: SearchField,

    /// Sort results by attr, name, or main-program. Append `-desc` for descending order.
    #[arg(long, value_enum, default_value_t = SearchSort::None)]
    sort: SearchSort,

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
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
}

/// Options for `nixdex stats`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct StatsOpts {
    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Print the statistics as JSON.
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

/// Options for `nixdex which`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct WhichOpts {
    /// Command to locate (e.g. `hello` or `/bin/hello`).
    #[arg(value_name = "COMMAND")]
    cmd: String,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Print all matching packages instead of the first one.
    #[arg(long)]
    all: bool,

    /// Print the result(s) as JSON.
    #[arg(long)]
    json: bool,
}

/// Options for `nixdex update`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct UpdateOpts {
    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Release URL pattern for nix-index-database.
    #[arg(
        long,
        default_value = "https://github.com/nix-community/nix-index-database/releases/latest/download"
    )]
    release_url: String,

    /// Architecture identifier (e.g., x86_64-linux).
    #[arg(long, default_value_t = nixdex_core::prebuilt::default_architecture())]
    architecture: String,

    /// Download the `-small` prebuilt variant.
    #[arg(long)]
    small: bool,
}

/// Options for `nixdex generate-sidecars`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct GenerateSidecarsOpts {
    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,
}

/// Options for `nixdex command-not-found`.
#[derive(Debug, Parser)]
#[command(author, about, version)]
struct CommandNotFoundOpts {
    /// Missing command to look up.
    #[arg(value_name = "COMMAND")]
    cmd: String,

    /// Arguments to pass to the command when --auto-install or --auto-run succeeds.
    #[arg(value_name = "ARGS", num_args = 0.., allow_hyphen_values = true)]
    args: Vec<String>,

    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value_os_t = nixdex_core::nixdex_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Automatically install the package and re-execute the command.
    #[arg(long)]
    auto_install: bool,

    /// Run the command once from the package without installing.
    #[arg(long)]
    auto_run: bool,

    /// Interactively prompt for a provider and run the command once without installing.
    #[arg(short = 'i', long)]
    interactive: bool,

    /// Output the suggestion as JSON.
    #[arg(long)]
    json: bool,
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
    #[arg(long, default_value_t = nixdex_core::prebuilt::default_architecture())]
    architecture: String,

    /// Use the -small variant of the prebuilt index.
    #[arg(long)]
    small: bool,

    /// Cache directory for prebuilt indexes.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Refresh interval in seconds.
    #[arg(long, default_value = "3600", value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,

    /// HTTP server listen address.
    #[arg(long, default_value = "127.0.0.1:3750")]
    http_addr: String,

    /// Serve an existing local index directory instead of downloading a prebuilt index.
    #[arg(long)]
    database: Option<PathBuf>,

    /// Bearer token required for `POST /reload` when not bound to loopback.
    /// If unset, `/reload` is only accepted from loopback addresses.
    #[arg(long, env = "NIXDEX_ADMIN_TOKEN")]
    admin_token: Option<String>,
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
        db.search_fuzzy(
            &opts.pattern,
            opts.field,
            opts.case_sensitive,
            opts.sort,
            opts.limit,
        )
    } else {
        db.search(
            &opts.pattern,
            opts.regex,
            opts.field,
            opts.case_sensitive,
            opts.exact,
            opts.sort,
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
        .search(
            &opts.attr,
            false,
            SearchField::Attr,
            true,
            true,
            SearchSort::None,
            None,
        )
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
        let main = record.main_program.as_deref().map_or("—", |m| m);
        println!("{}", record.attr);
        println!("  name:          {}", record.name);
        println!("  description:   {}", desc);
        println!("  main_program:  {}", main);
    }

    Ok(())
}

fn run_stats(opts: StatsOpts) -> color_eyre::Result<()> {
    let files = opts.database.join("files");
    if !files.is_file() {
        color_eyre::eyre::bail!("no database found at {}", files.display());
    }

    let reader = nixdex_core::database::Reader::open(&files)
        .wrap_err_with(|| format!("failed to open index at {}", files.display()))?;

    let file_size = std::fs::metadata(&files)?.len();
    let package_count = reader.package_count();

    let sidecar_names = [
        "files.basename.fst",
        "files.basename.postings",
        "files.basename.names",
        "files.packages.names",
        "files.path.fst",
        "files.path.postings",
        "files.attrs",
        "packages.json",
    ];
    let mut sidecar_sizes = std::collections::BTreeMap::new();
    for name in sidecar_names {
        let path = opts.database.join(name);
        let size = if path.is_file() {
            Some(std::fs::metadata(&path)?.len())
        } else {
            None
        };
        sidecar_sizes.insert(name, size);
    }

    if opts.json {
        #[derive(serde::Serialize)]
        struct StatsJson {
            version: u64,
            files_size: u64,
            package_count: Option<usize>,
            sidecars: std::collections::BTreeMap<&'static str, Option<u64>>,
        }
        let stats = StatsJson {
            version: reader.version(),
            files_size: file_size,
            package_count,
            sidecars: sidecar_sizes,
        };
        println!(
            "{}",
            sonic_rs::to_string(&stats).wrap_err("failed to serialize stats")?
        );
    } else {
        println!("database: {}", opts.database.display());
        println!("files: {} bytes (version {})", file_size, reader.version());
        if let Some(count) = package_count {
            println!("packages: {count}");
        } else {
            println!("packages: unknown");
        }
        println!("sidecars:");
        for (name, size) in sidecar_sizes {
            match size {
                Some(s) => println!("  {name}: {s} bytes"),
                None => println!("  {name}: missing"),
            }
        }
    }
    Ok(())
}

fn run_which(opts: WhichOpts) -> color_eyre::Result<()> {
    let files = opts.database.join("files");
    let reader = nixdex_core::database::Reader::open(&files)
        .wrap_err_with(|| format!("failed to open index at {}", files.display()))?;

    let providers = find_command_providers(&opts.cmd, &reader)?;
    let Some(first) = providers.first() else {
        color_eyre::eyre::bail!("no package found for command '{}'", opts.cmd);
    };

    if opts.json {
        let output = if opts.all {
            sonic_rs::to_string(&providers)
        } else {
            sonic_rs::to_string(first)
        }
        .wrap_err("failed to serialize which result")?;
        println!("{output}");
        return Ok(());
    }

    if opts.all {
        for provider in &providers {
            println!("{}", format_which_attr(provider));
        }
    } else {
        println!("{}", format_which_attr(first));
    }
    Ok(())
}

fn format_which_attr(store_path: &nixdex_core::StorePath) -> String {
    let mut attr = format!(
        "{}.{}",
        store_path.origin().attr,
        store_path.origin().output
    );
    if !store_path.origin().toplevel {
        attr = format!("({attr})");
    }
    attr
}

async fn run_update(opts: UpdateOpts) -> color_eyre::Result<()> {
    let config = nixdex_core::prebuilt::PrebuiltConfig {
        release_url: opts.release_url,
        architecture: opts.architecture,
        small: opts.small,
        cache_dir: opts.database.clone(),
        refresh_interval: std::time::Duration::ZERO,
    };
    let dest = opts.database.join("files");

    nixdex_core::prebuilt::download_to(&config, &dest)
        .await
        .wrap_err("failed to download prebuilt index")?;

    println!("updated index at {}", opts.database.display());
    Ok(())
}

fn run_generate_sidecars(opts: GenerateSidecarsOpts) -> color_eyre::Result<()> {
    let files = opts.database.join("files");
    nixdex_core::generate_sidecars(&files)
        .wrap_err_with(|| format!("failed to generate sidecars for {}", files.display()))?;
    println!("generated sidecars for {}", opts.database.display());
    Ok(())
}

fn find_command_providers(
    cmd: &str,
    reader: &nixdex_core::database::Reader,
) -> color_eyre::Result<Vec<nixdex_core::StorePath>> {
    let full_path = if cmd.starts_with('/') {
        cmd.to_string()
    } else {
        format!("/bin/{cmd}")
    };
    let pattern = format!("^{}$", regex::escape(&full_path));
    let re = regex::bytes::Regex::new(&pattern).wrap_err("invalid path pattern")?;

    let results = reader
        .search_entries(&re, None, None, None, None)
        .map_err(|err| color_eyre::eyre::eyre!("search failed: {err}"))?;

    let mut providers: Vec<nixdex_core::StorePath> = results
        .into_iter()
        .filter(|(_, entry)| {
            entry.node.is_executable()
                || matches!(entry.node, nixdex_core::files::FileNode::Symlink { .. })
        })
        .map(|(store_path, _)| store_path)
        .collect();

    // Prefer top-level packages over non-toplevel matches.
    providers.sort_by(|a, b| match (a.origin().toplevel, b.origin().toplevel) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });
    providers.dedup();
    Ok(providers)
}

#[allow(clippy::too_many_lines)]
fn run_command_not_found(opts: CommandNotFoundOpts) -> color_eyre::Result<()> {
    let auto_install =
        opts.auto_install || std::env::var("NIX_AUTO_INSTALL").is_ok_and(|v| !v.is_empty());
    let auto_run = opts.auto_run || std::env::var("NIX_AUTO_RUN").is_ok_and(|v| !v.is_empty());
    let interactive =
        opts.interactive || std::env::var("NIX_AUTO_RUN_INTERACTIVE").is_ok_and(|v| !v.is_empty());

    if [auto_install, auto_run, interactive]
        .iter()
        .filter(|&&v| v)
        .count()
        > 1
    {
        color_eyre::eyre::bail!(
            "--auto-install, --auto-run, and --interactive are mutually exclusive"
        );
    }

    let files = opts.database.join("files");
    let reader = nixdex_core::database::Reader::open(&files)
        .wrap_err_with(|| format!("failed to open index at {}", files.display()))?;

    let providers = find_command_providers(&opts.cmd, &reader)?;
    let comma = comma_available();

    // For execution we only need the command basename; an absolute path like
    // /bin/ls is not present inside the nix shell/profile.
    let exec_cmd = std::path::Path::new(&opts.cmd)
        .file_name()
        .and_then(|s| s.to_str())
        .map_or(opts.cmd.as_str(), |s| s);

    // Helper to get the attribute string for execution (only for top-level providers)
    let provider_attr = |sp: &nixdex_core::StorePath| -> color_eyre::Result<String> {
        if !sp.origin().toplevel {
            color_eyre::eyre::bail!(
                "cannot execute non-top-level provider '{}'",
                format_which_attr(sp)
            );
        }
        Ok(format!("{}.{}", sp.origin().attr, sp.origin().output))
    };

    match providers.as_slice() {
        [] => {
            eprintln!("{}: command not found", opts.cmd);
            std::process::exit(127);
        }
        [single] if interactive => {
            let attr = provider_attr(single)?;
            interactive_run(&[attr], exec_cmd, &opts.args)
        }
        _ if interactive => {
            let attrs: color_eyre::Result<Vec<String>> =
                providers.iter().map(provider_attr).collect();
            interactive_run(&attrs?, exec_cmd, &opts.args)
        }
        [single] if auto_install => {
            let attr = provider_attr(single)?;
            auto_install_and_exec(&attr, exec_cmd, &opts.args)
        }
        [single] if auto_run => {
            let attr = provider_attr(single)?;
            auto_run_command(&attr, exec_cmd, &opts.args)
        }
        [single] if opts.json => {
            let line =
                sonic_rs::to_string(single).wrap_err("failed to serialize command provider")?;
            println!("{line}");
            Ok(())
        }
        [single] => {
            let display = format_which_attr(single);
            eprintln!(
                "The program '{}' is currently not installed. It is provided by the package '{}'.",
                opts.cmd, display
            );
            if let Some(cmd) = comma {
                eprintln!("  Run it without installing: {cmd} {}", opts.cmd);
            }
            std::process::exit(127);
        }
        _ if auto_install || auto_run => {
            eprintln!(
                "The program '{}' is currently not installed. It is provided by several packages; cannot auto-install/run.",
                opts.cmd
            );
            for provider in &providers {
                eprintln!("  {}", format_which_attr(provider));
            }
            std::process::exit(127);
        }
        _ if opts.json => {
            let line = sonic_rs::to_string(&providers).wrap_err("failed to serialize providers")?;
            println!("{line}");
            Ok(())
        }
        _ => {
            eprintln!(
                "The program '{}' is currently not installed. It is provided by the following packages:",
                opts.cmd
            );
            for provider in &providers {
                eprintln!("  {}", format_which_attr(provider));
            }
            if let Some(cmd) = comma {
                eprintln!("  Run one without installing: {cmd} {}", opts.cmd);
            }
            std::process::exit(127);
        }
    }
}

fn auto_install_and_exec(provider: &str, cmd: &str, args: &[String]) -> color_eyre::Result<()> {
    let uses_nix_profile = std::env::var_os("HOME").is_some_and(|h| {
        std::path::PathBuf::from(h)
            .join(".nix-profile/manifest.json")
            .is_file()
    });

    let install_status = if uses_nix_profile {
        std::process::Command::new("nix")
            .args(["profile", "add", &format!("nixpkgs#{provider}")])
            .status()
            .wrap_err_with(|| format!("failed to run 'nix profile add nixpkgs#{provider}'"))?
    } else {
        // nix-env attribute paths do not accept output qualifiers like `.out`,
        // so strip the trailing output component for the legacy installer.
        let nix_env_attr = provider.rsplit_once('.').map_or(provider, |(attr, _)| attr);
        std::process::Command::new("nix-env")
            .args(["-iA", &format!("nixpkgs.{nix_env_attr}")])
            .status()
            .wrap_err_with(|| format!("failed to run 'nix-env -iA nixpkgs.{nix_env_attr}'"))?
    };

    if !install_status.success() {
        color_eyre::eyre::bail!("failed to install package '{provider}'");
    }

    exec_command(cmd, args)
}

fn auto_run_command(provider: &str, cmd: &str, args: &[String]) -> color_eyre::Result<()> {
    let mut command_args = vec![
        String::from("shell"),
        format!("nixpkgs#{provider}"),
        String::from("--command"),
        cmd.to_string(),
    ];
    command_args.extend_from_slice(args);

    let status = std::process::Command::new("nix")
        .args(&command_args)
        .status()
        .wrap_err_with(|| {
            format!("failed to run 'nix shell nixpkgs#{provider} --command {cmd}'")
        })?;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    std::process::exit(127);
}

fn interactive_run(providers: &[String], cmd: &str, args: &[String]) -> color_eyre::Result<()> {
    use std::io::Write;

    let provider = match providers {
        [single] => {
            eprint!("The program '{cmd}' is provided by the package '{single}'. Run it? [Y/n]: ");
            std::io::stderr().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let answer = input.trim().to_lowercase();
            if answer.is_empty() || answer == "y" || answer == "yes" {
                single.as_str()
            } else {
                std::process::exit(127);
            }
        }
        _ => {
            eprintln!("The program '{cmd}' is provided by several packages:");
            for (i, provider) in providers.iter().enumerate() {
                eprintln!("  {}. {provider}", i + 1);
            }
            eprint!("([Y]es | [number] | [n]one): ");
            std::io::stderr().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let answer = input.trim().to_lowercase();
            if answer.is_empty() || answer == "y" || answer == "yes" {
                providers
                    .first()
                    .map(String::as_str)
                    .ok_or_else(|| color_eyre::eyre::eyre!("no providers to run"))?
            } else if answer == "n" || answer == "none" {
                std::process::exit(127);
            } else if let Ok(index) = answer.parse::<usize>() {
                providers.get(index.saturating_sub(1)).map_or_else(
                    || {
                        eprintln!("invalid selection");
                        std::process::exit(127)
                    },
                    String::as_str,
                )
            } else {
                eprintln!("invalid selection");
                std::process::exit(127);
            }
        }
    };

    auto_run_command(provider, cmd, args)
}

fn exec_command(cmd: &str, args: &[String]) -> color_eyre::Result<()> {
    let status = std::process::Command::new(cmd)
        .args(args)
        .status()
        .wrap_err_with(|| format!("failed to execute '{cmd}'"))?;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    std::process::exit(127);
}

fn run_completions(opts: CompletionsOpts) {
    let mut cmd = Opts::command();
    let name = cmd.get_name().to_string();
    generate(opts.shell, &mut cmd, name, &mut std::io::stdout());
}

async fn run_daemon(opts: DaemonOpts) -> color_eyre::Result<()> {
    let cache_dir = opts
        .cache_dir
        .unwrap_or_else(|| nixdex_core::nixdex_dir().join("prebuilt"));
    let config = nixdex_core::daemon::DaemonConfig {
        prebuilt: nixdex_core::prebuilt::PrebuiltConfig {
            release_url: opts.release_url,
            architecture: opts.architecture,
            small: opts.small,
            cache_dir,
            refresh_interval: std::time::Duration::from_secs(opts.interval),
        },
        http_addr: opts.http_addr,
        local_database: opts.database,
        local_refresh_interval: std::time::Duration::from_secs(opts.interval),
        admin_token: opts.admin_token,
    };

    nixdex_core::daemon::run(&config)
        .await
        .wrap_err("nixdex-daemon failed")?;

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
        Cmd::Stats(stats_opts) => run_stats(stats_opts),
        Cmd::Completions(opts) => {
            run_completions(opts);
            Ok(())
        }
        Cmd::Index(index_opts) => index::run(index_opts).await,
        Cmd::Locate(locate_opts) => locate::run(locate_opts),
        Cmd::Which(which_opts) => run_which(which_opts),
        Cmd::Update(update_opts) => run_update(update_opts).await,
        Cmd::GenerateSidecars(opts) => run_generate_sidecars(opts),
        Cmd::CommandNotFound(opts) => run_command_not_found(opts),
        Cmd::Daemon(daemon_opts) => run_daemon(daemon_opts).await,
    }
}
