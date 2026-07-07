//! Querying available packages from nixpkgs via `nix-eval-jobs`.

use std::path::Path;
use std::process::Stdio;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Errors that can occur when querying nixpkgs.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// `nix-eval-jobs` or surrounding evaluation machinery failed.
    #[error("nix evaluation failed: {0}")]
    Evaluation(String),

    /// Local process I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias for this module.
pub type Result<T> = std::result::Result<T, Error>;

/// A list of package attribute paths discovered during evaluation.
#[derive(Debug, Clone, Default)]
pub struct PackageList {
    /// Fully-qualified attribute paths (for example `hello`).
    pub attrs: Vec<String>,
}

/// Options controlling a `nix-eval-jobs` invocation.
#[derive(Debug, Clone)]
pub struct EvalJobsOptions<'a> {
    /// Path or expression accepted by `nix-eval-jobs` (for example `<nixpkgs>`).
    pub nixpkgs: &'a str,
    /// Optional system triple passed through to evaluation.
    pub system: Option<&'a str>,
    /// Additional attribute scopes to walk.
    pub extra_scopes: &'a [String],
    /// Whether to pass `--show-trace` to Nix.
    pub show_trace: bool,
}

/// One NDJSON line produced by `nix-eval-jobs` (stub representation).
#[derive(Debug, Clone)]
pub struct EvalJobLine {
    /// Raw NDJSON line as emitted by `nix-eval-jobs`.
    pub raw: String,
}

/// Spawn `nix-eval-jobs` and collect NDJSON lines.
///
/// Scaffold: process spawning and line splitting are implemented so the
/// integration surface compiles. Full sonic-rs decoding of each record is
/// deferred until the schema mapping is finalized.
///
/// # Errors
///
/// Returns an error if the process cannot be started, exits unsuccessfully, or
/// produces unreadable output.
pub async fn run_eval_jobs(options: &EvalJobsOptions<'_>) -> Result<Vec<EvalJobLine>> {
    let mut cmd = Command::new("nix-eval-jobs");
    cmd.arg("--flake").arg(options.nixpkgs);
    if let Some(system) = options.system {
        cmd.arg("--system").arg(system);
    }
    if options.show_trace {
        cmd.arg("--show-trace");
    }
    for scope in options.extra_scopes {
        cmd.arg("--expr").arg(scope);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| {
        Error::Evaluation(format!("failed to spawn nix-eval-jobs: {err}"))
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        Error::Evaluation("nix-eval-jobs produced no stdout pipe".to_string())
    })?;

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut records = Vec::new();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        records.push(EvalJobLine { raw: line });
    }

    let status = child.wait().await?;
    if !status.success() {
        return Err(Error::Evaluation(format!(
            "nix-eval-jobs exited with status {status}"
        )));
    }

    Ok(records)
}

/// List packages available in a nixpkgs expression.
///
/// # Errors
///
/// Returns [`Error::NotImplemented`] until the full evaluator pipeline is ready.
pub fn list_packages(
    _nixpkgs: &str,
    _system: Option<&str>,
    _extra_scopes: &[String],
    _show_trace: bool,
) -> Result<PackageList> {
    Err(Error::NotImplemented(
        "list_packages is not implemented yet",
    ))
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
