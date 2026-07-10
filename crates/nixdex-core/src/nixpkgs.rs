//! Querying available packages from nixpkgs via `nix-eval-jobs`.

use std::path::Path;
use std::process::Stdio;

use indexmap::IndexMap;
use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

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
    /// Store paths discovered from successful evaluation lines.
    pub store_paths: Vec<StorePath>,
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
    /// Whether to pass `--check-cache-status`.
    pub check_cache_status: bool,
    /// Whether to pass `--show-trace` to Nix.
    pub show_trace: bool,
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
    #[serde(default)]
    pub outputs: IndexMap<String, String>,
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
}

fn default_store_dir() -> String {
    "/nix/store".to_string()
}

/// One NDJSON line produced by `nix-eval-jobs`.
#[derive(Debug, Clone)]
pub struct EvalJobLine {
    /// Raw NDJSON line as emitted by `nix-eval-jobs`.
    pub raw: String,
    /// Successfully decoded job, if the line was valid and non-error.
    pub job: Option<EvalJob>,
}

impl EvalJob {
    /// Whether this line is a successful derivation (not an error record).
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.error.is_none() && self.fatal != Some(true) && !self.outputs.is_empty()
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
        for (output, path) in &self.outputs {
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

    let job: EvalJob = sonic_rs::from_str(trimmed)
        .map_err(|err| Error::Json(format!("{err}: {trimmed}")))?;

    let job = if job.is_success() { Some(job) } else { None };
    Ok(EvalJobLine {
        raw: raw.to_string(),
        job,
    })
}

/// Spawn `nix-eval-jobs` and collect NDJSON lines (decoded when possible).
///
/// # Errors
///
/// Returns an error if the process cannot be started or exits unsuccessfully.
pub async fn run_eval_jobs(options: &EvalJobsOptions<'_>) -> Result<Vec<EvalJobLine>> {
    let mut cmd = Command::new("nix-eval-jobs");
    // Prefer flake-style only when the arg looks like a flake ref; otherwise
    // use --expr for classic `<nixpkgs>` / file paths.
    if options.nixpkgs.starts_with("github:")
        || options.nixpkgs.contains('#')
        || options.nixpkgs.starts_with('.')
    {
        cmd.arg("--flake").arg(options.nixpkgs);
    } else {
        cmd.arg("--expr").arg(format!(
            "import ({}) {{ config = {{ allowAliases = false; }}; }}",
            nix_string_literal(options.nixpkgs)
        ));
    }
    if let Some(system) = options.system {
        cmd.arg("--system").arg(system);
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
        // Soft-skip undecodable error lines rather than failing the whole stream.
        match parse_eval_line(&line) {
            Ok(record) => records.push(record),
            Err(_) => records.push(EvalJobLine {
                raw: line,
                job: None,
            }),
        }
    }

    let status = child.wait().await?;
    if !status.success() && records.is_empty() {
        return Err(Error::Evaluation(format!(
            "nix-eval-jobs exited with status {status}"
        )));
    }

    Ok(records)
}

/// List packages available in a nixpkgs expression (async evaluator path).
///
/// # Errors
///
/// Propagates evaluation errors from `nix-eval-jobs`.
pub async fn list_packages_async(options: &EvalJobsOptions<'_>) -> Result<PackageList> {
    let lines = run_eval_jobs(options).await?;
    let mut list = PackageList::default();
    for line in lines {
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
        list.attrs.push(attr);
        list.store_paths.extend(job.store_paths());
    }
    Ok(list)
}

/// Synchronous façade used by call sites that are not async yet.
///
/// # Errors
///
/// Returns evaluation errors from the async path.
pub fn list_packages(
    nixpkgs: &str,
    system: Option<&str>,
    _extra_scopes: &[String],
    show_trace: bool,
) -> Result<PackageList> {
    let options = EvalJobsOptions {
        nixpkgs,
        system,
        select: None,
        check_cache_status: true,
        show_trace,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(list_packages_async(&options))
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

fn nix_string_literal(value: &str) -> String {
    // `<nixpkgs>` is a path expression already.
    if value.starts_with('<') && value.ends_with('>') {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
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
        assert_eq!(paths[0].hash(), "pg2zfrrbm58ynbjshhzkgg4q466spinf");
        assert_eq!(paths[0].name(), "hello-2.12.3");
        assert_eq!(paths[0].origin().attr, "hello");
        assert_eq!(paths[0].origin().output, "out");
    }

    #[test]
    fn error_lines_yield_no_job() {
        let raw = r#"{"error":"unfree","fatal":false,"attr":"foo"}"#;
        let line = parse_eval_line(raw).expect("parse");
        assert!(line.job.is_none());
    }
}
