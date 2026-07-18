//! Querying available packages from nixpkgs via `nix-eval-jobs`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{ChildStderr, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::listings::PackageEntry;
use crate::store_path::{Origin, StorePath};

/// Errors that can occur when querying nixpkgs.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// `nix-eval-jobs` or surrounding evaluation machinery failed.
    #[error("nix evaluation failed: {0}")]
    Evaluation(String),

    /// A JSON line could not be decoded.
    #[error("invalid eval job JSON: {0}")]
    Json(String),

    /// Local process I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid user-supplied nixpkgs location / expression.
    #[error("invalid nixpkgs argument: {0}")]
    InvalidArgument(String),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias for this module.
pub type Result<T> = std::result::Result<T, Error>;

/// A single package discovered during evaluation, together with the store
/// paths produced by its outputs and optional `meta.mainProgram` value.
#[derive(Debug, Clone)]
pub struct Package {
    /// Fully-qualified attribute path (for example `hello`).
    pub attr: String,
    /// Store paths discovered from successful evaluation lines.
    pub store_paths: Vec<StorePath>,
    /// Value of `meta.mainProgram`, used to synthesize `/bin/<mainProgram>`
    /// listings for packages not available from the binary cache.
    pub main_program: Option<String>,
}

/// A list of packages discovered during evaluation.
#[derive(Debug, Clone, Default)]
pub struct PackageList {
    /// Fully-qualified attribute paths (for example `hello`).
    pub attrs: Vec<String>,
    /// Packages discovered from successful evaluation lines.
    pub packages: Vec<Package>,
}

/// Options controlling a `nix-eval-jobs` invocation.
#[derive(Debug, Clone)]
pub struct EvalJobsOptions<'a> {
    /// Path or expression accepted by `nix-eval-jobs` (for example `<nixpkgs>`).
    pub nixpkgs: &'a str,
    /// Optional system triple passed through to evaluation.
    pub system: Option<&'a str>,
    /// Optional `--select` expression to scope evaluation (recommended for tests).
    pub select: Option<&'a str>,
    /// Whether to pass `--no-instantiate` to `nix-eval-jobs`.
    ///
    /// This skips writing store derivations and can be significantly faster,
    /// but may leave `outputs` uninstantiated. It is most useful with
    /// metadata-only passes.
    pub no_instantiate: bool,
    /// Whether to pass `--check-cache-status`.
    pub check_cache_status: bool,
    /// Whether to pass `--show-trace` to Nix.
    pub show_trace: bool,
    /// Whether to pass `--meta` to `nix-eval-jobs` to fetch `meta.mainProgram`.
    pub meta: bool,
    /// Optional attribute path suffix for extra scopes (e.g. `haskellPackages`).
    /// When set, the evaluated expression becomes `(root).<scope>`.
    pub scope: Option<&'a str>,
    /// Whether to disable nixpkgs overlays (`overlays = []`).
    pub no_overlays: bool,
    /// Whether to allow unfree packages (`allowUnfree = true;`).
    pub allow_unfree: bool,
}

/// A subset of the `meta` attrset emitted by `nix-eval-jobs --meta`.
///
/// Only the fields nixdex cares about are captured; all other meta fields
/// are ignored during deserialization.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    /// The value of `meta.mainProgram`, if present and a string.
    #[serde(default)]
    pub main_program: Option<String>,
    /// The value of `meta.description`, if present and a string.
    #[serde(default)]
    pub description: Option<String>,
}

/// One successfully decoded derivation job from `nix-eval-jobs` NDJSON.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalJob {
    /// Short attribute name.
    pub attr: String,
    /// Full attribute path from the evaluation root.
    #[serde(default)]
    pub attr_path: Vec<String>,
    /// Derivation name, e.g. `hello-2.12.3`.
    #[serde(default)]
    pub name: String,
    /// Target system.
    #[serde(default)]
    pub system: String,
    /// Store directory, typically `/nix/store`.
    #[serde(default = "default_store_dir")]
    pub store_dir: String,
    /// Output name → store path map.
    ///
    /// Values may be `None` when `nix-eval-jobs` is run with `--no-instantiate`
    /// or when an output is not yet built.
    #[serde(default)]
    pub outputs: IndexMap<String, Option<String>>,
    /// Cache availability when `--check-cache-status` is set.
    #[serde(default)]
    pub cache_status: Option<String>,
    /// Whether outputs are available from a substituter or local store.
    #[serde(default)]
    pub is_cached: Option<bool>,
    /// Present on failure lines.
    #[serde(default)]
    pub error: Option<String>,
    /// Fatal evaluation failure flag.
    #[serde(default)]
    pub fatal: Option<bool>,
    /// Derivation `meta` attrset, present when `--meta` is passed to `nix-eval-jobs`.
    #[serde(default)]
    pub meta: Option<Meta>,
}

fn default_store_dir() -> String {
    "/nix/store".to_string()
}

/// A small, human-readable package record extracted from a `nix-eval-jobs` line.
///
/// This is written as a sidecar (`packages.json`) during indexing and searched
/// by `nixdex search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageMeta {
    /// Attribute path, e.g. `nixpkgs#hello` or `hello`.
    pub attr: String,
    /// Derivation name, e.g. `hello-2.12.3`.
    pub name: String,
    /// `meta.description`, if present.
    pub description: Option<String>,
    /// `meta.mainProgram`, if present.
    pub main_program: Option<String>,
}

/// One NDJSON line produced by `nix-eval-jobs`.
#[derive(Debug, Clone)]
pub struct EvalJobLine {
    /// Raw NDJSON line as emitted by `nix-eval-jobs`.
    pub raw: String,
    /// Successfully decoded job, if the line was valid and non-error.
    pub job: Option<EvalJob>,
}

/// Streaming output of a running `nix-eval-jobs` invocation.
///
/// Receive parsed NDJSON lines from [`rx`](Self::rx) and await
/// [`handle`](Self::handle) to learn whether the process exited cleanly.
#[derive(Debug)]
pub struct EvalJobsStream {
    /// Parsed NDJSON lines as they are emitted.
    pub rx: mpsc::Receiver<EvalJobLine>,
    /// Completion handle for the process reader task.
    pub handle: tokio::task::JoinHandle<Result<()>>,
}

impl EvalJob {
    /// Whether this line is a successful derivation (not an error record).
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.error.is_none() && self.fatal != Some(true)
    }

    /// Whether the outputs look available for substitution / listing.
    #[must_use]
    pub fn is_available(&self) -> bool {
        if !self.is_success() {
            return false;
        }
        // Without cache status, treat as available and let .ls 404 filter later.
        matches!(
            (self.is_cached, self.cache_status.as_deref()),
            (Some(true), _) | (_, Some("cached" | "local")) | (None, None)
        )
    }

    /// Convert outputs into `StorePath` values with origin metadata.
    #[must_use]
    pub fn store_paths(&self) -> Vec<StorePath> {
        let attr = if self.attr_path.is_empty() {
            self.attr.clone()
        } else {
            self.attr_path.join(".")
        };
        let system = if self.system.is_empty() {
            None
        } else {
            Some(self.system.clone())
        };

        let mut out = Vec::with_capacity(self.outputs.len());
        for (output, maybe_path) in &self.outputs {
            let Some(path) = maybe_path else {
                continue;
            };
            let origin = Origin {
                attr: attr.clone(),
                output: output.clone(),
                toplevel: true,
                system: system.clone(),
            };
            if let Some(sp) = StorePath::parse(origin, path) {
                out.push(sp);
            }
        }
        out
    }

    /// Return the value of `meta.mainProgram`, if any.
    #[must_use]
    pub fn main_program(&self) -> Option<&str> {
        self.meta.as_ref()?.main_program.as_deref()
    }

    /// Return the value of `meta.description`, if any.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.meta.as_ref()?.description.as_deref()
    }
}

/// Decode a single NDJSON line into an [`EvalJobLine`].
///
/// # Errors
///
/// Returns [`Error::Json`] when the line is not valid JSON.
pub fn parse_eval_line(raw: &str) -> Result<EvalJobLine> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(EvalJobLine {
            raw: raw.to_string(),
            job: None,
        });
    }

    let job: EvalJob =
        sonic_rs::from_str(trimmed).map_err(|err| Error::Json(format!("{err}: {trimmed}")))?;

    let job = if job.is_success() { Some(job) } else { None };
    Ok(EvalJobLine {
        raw: raw.to_string(),
        job,
    })
}

/// Escape a filesystem path so it can be safely embedded in a Nix double-quoted
/// string literal used in a `nix-eval-jobs --expr` argument.
fn nix_string_escape(s: &str) -> String {
    // Backslashes must be escaped first so the later escapes are not doubled.
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${")
}

/// Validate a scope attribute path component sequence (`haskellPackages`, `a.b`).
fn validate_scope(scope: &str) -> Result<()> {
    if scope.is_empty() {
        return Err(Error::InvalidArgument("scope cannot be empty".into()));
    }
    for part in scope.split('.') {
        if part.is_empty()
            || !part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || part.starts_with('-')
        {
            return Err(Error::InvalidArgument(format!(
                "invalid scope component in {scope:?}"
            )));
        }
    }
    Ok(())
}

/// Build a `nix-eval-jobs --expr` argument for a trusted nixpkgs location.
///
/// Allowed forms only:
/// - `<nixpkgs>` / other pure path lookups of the form `<name>`
/// - An existing filesystem path to a `.nix` file (canonicalized)
/// - An existing filesystem directory used as a classic nixpkgs root
///
/// Free-form string expressions are **rejected** to prevent injecting Nix code
/// through `-f` / `--nixpkgs`.
///
/// # Errors
///
/// Returns [`Error::InvalidArgument`] for rejected inputs.
pub fn eval_expr_for_nixpkgs(
    value: &str,
    scope: Option<&str>,
    no_overlays: bool,
    allow_unfree: bool,
) -> Result<String> {
    if let Some(scope) = scope {
        validate_scope(scope)?;
    }

    let mut config_entries = Vec::from(["allowAliases = false;"]);
    if allow_unfree {
        config_entries.push("allowUnfree = true;");
    }
    let config = config_entries.join(" ");
    let overlays = if no_overlays { "overlays = []; " } else { "" };

    let root = if let Some(inner) = value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) {
        // Only allow simple alphanumeric path lookups, not `<foo/../../../x>`.
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(Error::InvalidArgument(format!(
                "invalid path lookup {value:?}"
            )));
        }
        format!("import {value} {{ config = {{ {config} }}; {overlays}}}")
    } else {
        let path = Path::new(value);
        let abs: PathBuf = path.canonicalize().map_err(|err| {
            Error::InvalidArgument(format!(
                "nixpkgs path must exist and be resolvable: {value}: {err}"
            ))
        })?;
        let abs_str = abs.as_os_str().to_string_lossy();
        let escaped = nix_string_escape(&abs_str);
        if abs.is_file() {
            // File roots are evaluated as-is (fixtures like research/small.nix).
            format!("import \"{escaped}\"")
        } else if abs.is_dir() {
            format!("import \"{escaped}\" {{ config = {{ {config} }}; {overlays}}}")
        } else {
            return Err(Error::InvalidArgument(format!(
                "nixpkgs path is neither a file nor a directory: {}",
                abs.display()
            )));
        }
    };

    match scope {
        Some(scope) => Ok(format!("({root}).{scope}")),
        None => Ok(root),
    }
}

/// Spawn `nix-eval-jobs` and stream NDJSON lines as they are emitted.
///
/// The returned [`EvalJobsStream`] yields decoded lines through its receiver.
/// Await the `handle` after consuming the receiver to propagate process
/// errors.
///
/// # Errors
///
/// Returns an error if the process cannot be started.
pub async fn run_eval_jobs(options: &EvalJobsOptions<'_>) -> Result<EvalJobsStream> {
    let mut cmd = Command::new("nix-eval-jobs");
    // Flake refs only when they look like flake URLs, never for bare paths.
    if options.nixpkgs.starts_with("github:")
        || options.nixpkgs.starts_with("git+")
        || options.nixpkgs.starts_with("path:")
        || (options.nixpkgs.contains('#')
            && (options.nixpkgs.starts_with('.') || options.nixpkgs.starts_with('/')))
    {
        if options.scope.is_some() {
            return Err(Error::InvalidArgument(
                "extra scopes are not supported with flake refs yet".into(),
            ));
        }
        // Still reject nested # injection tricks with whitespace / shell metacharacters.
        if options
            .nixpkgs
            .chars()
            .any(|c| c.is_whitespace() || ";|&$`".contains(c))
        {
            return Err(Error::InvalidArgument(
                "flake ref contains disallowed characters".into(),
            ));
        }
        cmd.arg("--flake").arg(options.nixpkgs);
    } else {
        let expr = eval_expr_for_nixpkgs(
            options.nixpkgs,
            options.scope,
            options.no_overlays,
            options.allow_unfree,
        )?;
        cmd.arg("--expr").arg(expr);
    }
    if let Some(system) = options.system {
        cmd.arg("--system").arg(system);
    }
    if options.no_instantiate {
        cmd.arg("--no-instantiate");
    }
    if let Some(select) = options.select {
        cmd.arg("--select").arg(select);
    }
    if options.check_cache_status {
        cmd.arg("--check-cache-status");
    }
    if options.show_trace {
        cmd.arg("--show-trace");
    }
    if options.meta {
        // `meta.mainProgram` is needed to synthesize `/bin/<mainProgram>` listings.
        cmd.arg("--meta");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|err| Error::Evaluation(format!("failed to spawn nix-eval-jobs: {err}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Evaluation("nix-eval-jobs produced no stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Evaluation("nix-eval-jobs produced no stderr pipe".to_string()))?;

    let (tx, rx) = mpsc::channel(1024);

    let stdout_handle = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut records = 0usize;

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let record = match parse_eval_line(&line) {
                Ok(record) => record,
                Err(_) => EvalJobLine {
                    raw: line,
                    job: None,
                },
            };
            if tx.send(record).await.is_err() {
                // The consumer dropped; kill the child process and stop reading.
                let _ = child.kill().await;
                break;
            }
            records += 1;
        }

        let status = child.wait().await?;
        if !status.success() && records == 0 {
            return Err(Error::Evaluation(format!(
                "nix-eval-jobs exited with status {status}"
            )));
        }
        Ok(())
    });

    let stderr_handle = spawn_stderr_reader(stderr);
    let handle =
        tokio::spawn(async move { combine_eval_results(stdout_handle, stderr_handle).await });

    Ok(EvalJobsStream { rx, handle })
}

/// Spawn a task that drains the `nix-eval-jobs` stderr pipe into a string.
fn spawn_stderr_reader(stderr: ChildStderr) -> JoinHandle<String> {
    tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut reader = BufReader::new(stderr);
        let _ = AsyncReadExt::read_to_end(&mut reader, &mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Combine the stdout reader result with captured stderr for clearer errors.
async fn combine_eval_results(
    stdout_handle: JoinHandle<Result<()>>,
    stderr_handle: JoinHandle<String>,
) -> Result<()> {
    let (stdout_result, stderr_result) = tokio::join!(stdout_handle, stderr_handle);
    let stderr_text = match stderr_result {
        Ok(text) => text,
        Err(err) => format!("stderr reader task panicked: {err}"),
    };
    match stdout_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            let suffix = if stderr_text.trim().is_empty() {
                String::new()
            } else {
                format!("\nnix-eval-jobs stderr:\n{stderr_text}")
            };
            Err(Error::Evaluation(format!("{err}{suffix}")))
        }
        Err(err) => Err(Error::Evaluation(format!(
            "eval reader task panicked: {err}"
        ))),
    }
}

/// Await an eval-reader [`tokio::task::JoinHandle`] and map a panic into an
/// [`Error::Evaluation`].
async fn await_eval_handle(handle: tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    match handle.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(Error::Evaluation(format!(
            "eval reader task panicked: {err}"
        ))),
    }
}

/// List packages available in a nixpkgs expression (async evaluator path).
///
/// # Errors
///
/// Propagates evaluation errors from `nix-eval-jobs`.
pub async fn list_packages_async(options: &EvalJobsOptions<'_>) -> Result<PackageList> {
    let mut stream = run_eval_jobs(options).await?;
    let mut list = PackageList::default();
    while let Some(line) = stream.rx.recv().await {
        let Some(job) = line.job else {
            continue;
        };
        if !job.is_available() {
            continue;
        }
        let attr = if job.attr_path.is_empty() {
            job.attr.clone()
        } else {
            job.attr_path.join(".")
        };
        let main_program = job.main_program().map(String::from);
        list.attrs.push(attr.clone());
        list.packages.push(Package {
            attr,
            store_paths: job.store_paths(),
            main_program,
        });
    }
    await_eval_handle(stream.handle).await?;
    Ok(list)
}

/// Stream the decoded NDJSON lines of a single `nix-eval-jobs` run into
/// [`PackageEntry`] values.
///
/// `main_program` is only attached to the `out` output, matching the behaviour
/// of `build_starting_set`.
async fn stream_options_to_entries(
    options: &EvalJobsOptions<'_>,
    tx: &mpsc::Sender<PackageEntry>,
    meta_tx: Option<&mpsc::Sender<PackageMeta>>,
    main_program: bool,
) -> Result<usize> {
    if tx.is_closed() {
        return Ok(0);
    }
    let mut stream = run_eval_jobs(options).await?;
    let mut count = 0;
    while let Some(line) = stream.rx.recv().await {
        let Some(job) = line.job else {
            continue;
        };
        if !job.is_available() {
            continue;
        }
        let attr = if job.attr_path.is_empty() {
            job.attr.clone()
        } else {
            job.attr_path.join(".")
        };
        if let Some(meta_tx) = meta_tx {
            let meta = PackageMeta {
                attr: attr.clone(),
                name: job.name.clone(),
                description: job.description().map(String::from),
                main_program: job.main_program().map(String::from),
            };
            if meta_tx.send(meta).await.is_err() {
                // Consumer dropped; stop reading.
                return Ok(count);
            }
        }
        let main = if main_program {
            job.main_program().map(String::from)
        } else {
            None
        };
        for path in job.store_paths() {
            let entry_main = if path.origin().output == "out" {
                main.clone()
            } else {
                None
            };
            let entry = PackageEntry {
                path,
                main_program: entry_main,
            };
            if tx.send(entry).await.is_err() {
                // Consumer dropped; stop reading.
                return Ok(count);
            }
            count += 1;
        }
    }
    await_eval_handle(stream.handle).await?;
    Ok(count)
}

/// Stream evaluated package entries for the root set plus each extra scope.
///
/// Root evaluation failures are propagated; missing or failing extra scopes are
/// skipped so custom nixpkgs without `haskellPackages` etc. still work.
///
/// # Errors
///
/// Propagates evaluation errors for the root set.
pub async fn stream_package_entries(
    base: EvalJobsOptions<'_>,
    extra_scopes: &[String],
    main_program: bool,
    tx: mpsc::Sender<PackageEntry>,
    meta_tx: mpsc::Sender<PackageMeta>,
) -> Result<usize> {
    let meta_ref = Some(&meta_tx);
    let mut count = stream_options_to_entries(&base, &tx, meta_ref, main_program).await?;

    // A `--select` expression is applied to the root nixpkgs set; extra scopes
    // are separate attribute paths and should not be evaluated when the caller
    // has explicitly narrowed the root set.
    if base.select.is_some() {
        return Ok(count);
    }

    for scope in extra_scopes {
        if scope.is_empty() {
            continue;
        }
        if validate_scope(scope).is_err() {
            continue;
        }
        // Apply the root `--select` only to the root set; scopes are already
        // narrowed subsets.
        let scope_opts = EvalJobsOptions {
            nixpkgs: base.nixpkgs,
            system: base.system,
            select: None,
            no_instantiate: base.no_instantiate,
            check_cache_status: base.check_cache_status,
            show_trace: base.show_trace,
            meta: base.meta,
            scope: Some(scope.as_str()),
            no_overlays: base.no_overlays,
            allow_unfree: base.allow_unfree,
        };
        match stream_options_to_entries(&scope_opts, &tx, meta_ref, main_program).await {
            Ok(n) => count += n,
            Err(_) => {
                // Soft-skip missing scopes (custom nixpkgs without haskellPackages).
            }
        }
    }

    Ok(count)
}

/// List root packages plus each non-empty extra scope (sequential eval passes).
///
/// # Errors
///
/// Propagates hard evaluation failures for the root set. Scope failures are
/// returned as soft-empty lists by the caller.
pub async fn list_packages_with_scopes(
    base: EvalJobsOptions<'_>,
    extra_scopes: &[String],
) -> Result<PackageList> {
    let mut merged = list_packages_async(&base).await?;

    // A `--select` expression is applied to the root nixpkgs set; extra scopes
    // are separate attribute paths and should not be evaluated when the caller
    // has explicitly narrowed the root set.
    if base.select.is_some() {
        return Ok(merged);
    }

    for scope in extra_scopes {
        if scope.is_empty() {
            continue;
        }
        if validate_scope(scope).is_err() {
            continue;
        }
        let scope_opts = EvalJobsOptions {
            nixpkgs: base.nixpkgs,
            system: base.system,
            select: None,
            no_instantiate: base.no_instantiate,
            check_cache_status: base.check_cache_status,
            show_trace: base.show_trace,
            meta: base.meta,
            scope: Some(scope.as_str()),
            no_overlays: base.no_overlays,
            allow_unfree: base.allow_unfree,
        };
        match list_packages_async(&scope_opts).await {
            Ok(more) => {
                merged.attrs.extend(more.attrs);
                merged.packages.extend(more.packages);
            }
            Err(_) => {
                // Soft-skip missing scopes (custom nixpkgs without haskellPackages).
            }
        }
    }
    Ok(merged)
}

/// Synchronous façade used by call sites that are not async yet.
///
/// # Errors
///
/// Returns evaluation errors from the async path.
pub fn list_packages(
    nixpkgs: &str,
    system: Option<&str>,
    extra_scopes: &[String],
    show_trace: bool,
    main_program: bool,
) -> Result<PackageList> {
    let base = EvalJobsOptions {
        nixpkgs,
        system,
        select: None,
        no_instantiate: false,
        check_cache_status: true,
        show_trace,
        meta: main_program,
        scope: None,
        no_overlays: false,
        allow_unfree: false,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(list_packages_with_scopes(base, extra_scopes))
}

/// Verify that a local nixpkgs path exists (pre-flight helper).
///
/// # Errors
///
/// Returns an evaluation error when the path is missing.
pub fn ensure_nixpkgs_path(path: &Path) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        Err(Error::Evaluation(format!(
            "nixpkgs path does not exist: {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hello_eval_line() {
        let raw = r#"{"attr":"hello","attrPath":["hello"],"cacheStatus":"local","drvPath":"/nix/store/x.drv","isCached":true,"name":"hello-2.12.3","outputs":{"out":"/nix/store/pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3"},"storeDir":"/nix/store","system":"x86_64-linux"}"#;
        let line = parse_eval_line(raw).expect("parse");
        let job = line.job.expect("job");
        assert!(job.is_success());
        assert!(job.is_available());
        let paths = job.store_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths.first().map(StorePath::hash),
            Some("pg2zfrrbm58ynbjshhzkgg4q466spinf")
        );
    }

    #[test]
    fn parse_main_program_from_meta() {
        let raw = r#"{"attr":"nh","attrPath":["nh"],"cacheStatus":"local","drvPath":"/nix/store/x.drv","isCached":true,"name":"nh-4.3.2","meta":{"mainProgram":"nh","description":"Nix helper"},"outputs":{"out":"/nix/store/k9c04vx63x91fa3k147g5hi7k0ppns80-nh-4.3.2"},"storeDir":"/nix/store","system":"x86_64-linux"}"#;
        let line = parse_eval_line(raw).expect("parse");
        let job = line.job.expect("job");
        assert_eq!(job.main_program(), Some("nh"));
    }

    #[test]
    fn main_program_missing_when_meta_absent() {
        let raw = r#"{"attr":"hello","attrPath":["hello"],"cacheStatus":"local","drvPath":"/nix/store/x.drv","isCached":true,"name":"hello-2.12.3","outputs":{"out":"/nix/store/pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3"},"storeDir":"/nix/store","system":"x86_64-linux"}"#;
        let line = parse_eval_line(raw).expect("parse");
        let job = line.job.expect("job");
        assert!(job.main_program().is_none());
    }

    #[test]
    fn parse_description_from_meta() {
        let raw = r#"{"attr":"nh","attrPath":["nh"],"cacheStatus":"local","drvPath":"/nix/store/x.drv","isCached":true,"name":"nh-4.3.2","meta":{"mainProgram":"nh","description":"Nix helper"},"outputs":{"out":"/nix/store/k9c04vx63x91fa3k147g5hi7k0ppns80-nh-4.3.2"},"storeDir":"/nix/store","system":"x86_64-linux"}"#;
        let line = parse_eval_line(raw).expect("parse");
        let job = line.job.expect("job");
        assert_eq!(job.description(), Some("Nix helper"));
    }

    #[test]
    fn error_lines_yield_no_job() {
        let raw = r#"{"error":"unfree","fatal":false,"attr":"foo"}"#;
        let line = parse_eval_line(raw).expect("parse");
        assert!(line.job.is_none());
    }

    #[test]
    fn rejects_injection_in_nixpkgs_arg() {
        assert!(eval_expr_for_nixpkgs(r"$(rm -rf /)", None, false, false).is_err());
        assert!(
            eval_expr_for_nixpkgs(r#"foo"; builtins.trace "x" 1#"#, None, false, false).is_err()
        );
        assert!(eval_expr_for_nixpkgs("<nixpkgs/../../etc>", None, false, false).is_err());
    }

    #[test]
    fn accepts_path_lookup_and_scope() {
        let expr =
            eval_expr_for_nixpkgs("<nixpkgs>", Some("haskellPackages"), false, false).expect("ok");
        assert!(expr.contains("import <nixpkgs>"));
        assert!(expr.ends_with(".haskellPackages"));
    }

    #[test]
    fn rejects_bad_scope() {
        assert!(eval_expr_for_nixpkgs("<nixpkgs>", Some("foo;bar"), false, false).is_err());
        assert!(eval_expr_for_nixpkgs("<nixpkgs>", Some("../x"), false, false).is_err());
    }

    #[test]
    fn no_overlays_adds_empty_overlays() {
        let expr = eval_expr_for_nixpkgs("<nixpkgs>", None, true, false).expect("ok");
        assert!(expr.contains("import <nixpkgs>"));
        assert!(expr.contains("overlays = []"));
    }

    #[test]
    fn allow_unfree_adds_config_entry() {
        let expr = eval_expr_for_nixpkgs("<nixpkgs>", None, false, true).expect("ok");
        assert!(expr.contains("import <nixpkgs>"));
        assert!(expr.contains("allowUnfree = true;"));
    }

    #[test]
    fn escapes_nix_metacharacters_in_file_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_name = r#"test"; ${builtins.trace "x" 1}.nix"#;
        let path = dir.path().join(file_name);
        std::fs::File::create(&path).expect("create file");

        let expr = eval_expr_for_nixpkgs(path.to_str().expect("utf-8 path"), None, false, false)
            .expect("build expression");

        assert!(expr.starts_with("import \""), "expr: {expr}");
        assert!(expr.contains(r#"\""#), "expr: {expr}");
        assert!(expr.contains(r"\${builtins"), "expr: {expr}");
    }
}
