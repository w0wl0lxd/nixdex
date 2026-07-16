//! Building a nixdex index from nixpkgs and the binary cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::CACHE_URL;
use crate::database::Writer;
use crate::errors::{Error, Result};
use crate::hydra::Fetcher;
use crate::listings;
use crate::nixpkgs;
use crate::path_cache::PathCache;

/// Options controlling an index build.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Number of concurrent HTTP requests.
    pub jobs: usize,
    /// Directory where the index database is stored.
    pub database: PathBuf,
    /// nixpkgs path/expression (for example `<nixpkgs>`).
    pub nixpkgs: String,
    /// Optional system triple for evaluation.
    pub system: Option<String>,
    /// Zstandard compression level for the on-disk database.
    pub compression_level: i32,
    /// On-disk database format version (1 or 2).
    pub format_version: u64,
    /// Pass `--show-trace` to Nix evaluation.
    pub show_trace: bool,
    /// Only index paths starting with this prefix.
    pub filter_prefix: String,
    /// Persist intermediate fetch results into `paths.cache`.
    pub path_cache: bool,
    /// Ignore the existing `paths.cache` and re-fetch all store paths.
    pub force: bool,
    /// Cache-key used to identify a `paths.cache` file; defaults to `nixpkgs`.
    pub cache_key: Option<String>,
    /// Path to the `paths.cache` file; defaults to `<database>/paths.cache`.
    pub path_cache_file: Option<PathBuf>,
    /// Time-to-live for cache entries in seconds (0 = no expiry); defaults to 7 days.
    pub path_cache_ttl: Option<u64>,
    /// Synthesize `/bin/<mainProgram>` listings from `meta.mainProgram`.
    pub main_program: bool,
    /// Extra attribute scopes to walk during evaluation.
    pub extra_scopes: Vec<String>,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            jobs: 100,
            database: PathBuf::from("/tmp/nix-index"),
            nixpkgs: String::from("<nixpkgs>"),
            system: None,
            compression_level: 19,
            format_version: 2,
            show_trace: false,
            filter_prefix: String::new(),
            path_cache: false,
            force: false,
            cache_key: None,
            path_cache_file: None,
            path_cache_ttl: None,
            main_program: true,
            extra_scopes: vec![
                String::from("haskellPackages"),
                String::from("rPackages"),
                String::from("coqPackages"),
                String::from("texlive.pkgs"),
            ],
        }
    }
}

/// Orchestrates eval → fetch → write for a nixdex index.
#[derive(Debug, Default)]
pub struct IndexBuilder {
    options: UpdateOptions,
}

impl IndexBuilder {
    /// Create a builder with the supplied options.
    #[must_use]
    pub fn new(options: UpdateOptions) -> Self {
        Self { options }
    }

    /// Borrow the configured options.
    #[must_use]
    pub fn options(&self) -> &UpdateOptions {
        &self.options
    }

    /// Spawn an async task that evaluates nixpkgs and streams
    /// [`PackageEntry`] values into a channel.
    ///
    /// Returns the task handle and the receiver to be passed to the listing
    /// fetcher.
    fn spawn_package_eval_stream(
        &self,
    ) -> (
        tokio::task::JoinHandle<nixpkgs::Result<(usize, Duration)>>,
        mpsc::Receiver<listings::PackageEntry>,
    ) {
        let opts = &self.options;
        let (pkg_tx, pkg_rx) = mpsc::channel(1024);
        let nixpkgs_expr = opts.nixpkgs.clone();
        let system = opts.system.clone();
        let extra_scopes = opts.extra_scopes.clone();
        let show_trace = opts.show_trace;
        let main_program = opts.main_program;

        let handle = tokio::spawn(async move {
            let start = Instant::now();
            let count = nixpkgs::stream_package_entries(
                &nixpkgs_expr,
                system.as_deref(),
                &extra_scopes,
                show_trace,
                main_program,
                pkg_tx,
            )
            .await?;
            Ok((count, start.elapsed()))
        });

        (handle, pkg_rx)
    }

    /// Prepare the output database directory, `paths.cache`, and `Writer`.
    fn prepare_database(&self) -> Result<(PathBuf, Writer, Option<Arc<PathCache>>, PathBuf)> {
        let opts = &self.options;

        std::fs::create_dir_all(&opts.database).map_err(|source| Error::CreateDatabaseDir {
            path: opts.database.clone(),
            source,
        })?;

        let cache_path = opts
            .path_cache_file
            .clone()
            .unwrap_or_else(|| opts.database.join("paths.cache"));
        let cache_key = match opts.cache_key.as_deref() {
            Some(key) => key,
            None => &opts.nixpkgs,
        };
        let path_cache = load_path_cache(
            &cache_path,
            cache_key,
            opts.path_cache,
            opts.force,
            opts.path_cache_ttl,
        );

        let db_file = opts.database.join("files");
        let writer =
            Writer::create_with_version(&db_file, opts.compression_level, opts.format_version)
                .map_err(|source| Error::CreateDatabase {
                    path: db_file.clone(),
                    source: Box::new(source),
                })?;

        Ok((db_file, writer, path_cache, cache_path))
    }

    /// Build a fresh binary-cache fetcher.
    fn new_fetcher() -> Result<Fetcher> {
        Fetcher::new(CACHE_URL).map_err(|err| {
            Error::Io(std::io::Error::other(format!(
                "failed to create binary-cache client: {err}"
            )))
        })
    }

    /// Await the eval stream task and translate errors into the workspace type.
    async fn await_eval(
        handle: tokio::task::JoinHandle<nixpkgs::Result<(usize, Duration)>>,
    ) -> Result<(usize, Duration)> {
        match handle.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(source)) => Err(Error::QueryPackages {
                source: Box::new(source),
            }),
            Err(err) => Err(Error::Io(std::io::Error::other(format!(
                "eval task panicked: {err}"
            )))),
        }
    }

    /// Persist the `paths.cache` sidecar if the user enabled it.
    #[allow(clippy::cognitive_complexity)]
    fn maybe_save_path_cache(
        enabled: bool,
        path_cache: Option<&Arc<PathCache>>,
        cache_path: &Path,
    ) {
        if !enabled {
            return;
        }
        match path_cache {
            Some(pc) => {
                if let Err(err) = pc.save(cache_path) {
                    warn!(error = %err, "failed to save path cache");
                }
            }
            None => warn!("path cache enabled but not initialized"),
        }
    }

    /// Run the index build: evaluate packages, fetch `.ls` trees, write NIXI DB.
    ///
    /// # Errors
    ///
    /// Returns an error if the database directory cannot be created, evaluation
    /// fails hard, or the writer cannot be finalized.
    pub async fn build(&self) -> Result<()> {
        let opts = &self.options;

        let (db_file, mut writer, path_cache, cache_path) = self.prepare_database()?;
        let progress = MultiProgress::new();
        let eval_pb = progress.add(ProgressBar::new_spinner());
        eval_pb.set_message("Evaluating nixpkgs...");

        let (eval_handle, pkg_rx) = self.spawn_package_eval_stream();
        let fetcher = Self::new_fetcher()?;
        let filter_prefix = opts.filter_prefix.as_bytes().to_vec();
        let (indexed, failed, fetch_elapsed) = match write_listings(
            &mut writer,
            &fetcher,
            opts.jobs.max(1),
            pkg_rx,
            path_cache.clone(),
            filter_prefix,
            &db_file,
            &progress,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => {
                eval_handle.abort();
                return Err(err);
            }
        };

        let (eval_count, eval_elapsed) = Self::await_eval(eval_handle).await?;
        eval_pb.finish_with_message(format!(
            "Evaluated {eval_count} package(s) in {eval_elapsed:?}"
        ));

        Self::maybe_save_path_cache(opts.path_cache, path_cache.as_ref(), &cache_path);

        let size = writer.finish().map_err(|source| Error::WriteDatabase {
            path: db_file.clone(),
            source: Box::new(source),
        })?;

        let cached = path_cache
            .as_ref()
            .map_or(0, |pc| pc.hits.load(Ordering::Relaxed));
        let bytes_downloaded = fetcher.bytes_downloaded();

        info!(
            indexed,
            failed,
            cached,
            bytes_downloaded,
            size_bytes = size,
            ?eval_elapsed,
            ?fetch_elapsed,
            db = %db_file.display(),
            "index build complete"
        );
        Ok(())
    }
}

#[allow(clippy::cognitive_complexity)]
fn load_path_cache(
    cache_path: &Path,
    cache_key: &str,
    enabled: bool,
    force: bool,
    ttl_secs: Option<u64>,
) -> Option<Arc<PathCache>> {
    if !enabled {
        return None;
    }
    let ttl = match ttl_secs {
        Some(t) => t,
        None => 7 * 24 * 60 * 60, // 7 days default
    };
    if force {
        return Some(Arc::new(PathCache::new_with_ttl(cache_key, ttl)));
    }
    match PathCache::load(cache_path, cache_key) {
        Ok(Some(cache)) => Some(Arc::new(cache)),
        Ok(None) => {
            info!(cache_key, "path cache not found or stale; starting fresh");
            Some(Arc::new(PathCache::new_with_ttl(cache_key, ttl)))
        }
        Err(err) => {
            warn!(error = %err, "failed to load path cache; starting fresh");
            Some(Arc::new(PathCache::new_with_ttl(cache_key, ttl)))
        }
    }
}

/// Flush the in-memory writer chunk once it exceeds the configured threshold.
fn maybe_flush_chunk(writer: &mut Writer, db_file: &Path, chunk_bytes: u64) -> Result<()> {
    if writer.estimated_size() > chunk_bytes {
        tracing::debug!(
            estimated_bytes = writer.estimated_size(),
            "chunk size reached, flushing v2 frame"
        );
        writer
            .flush_chunk()
            .map_err(|source| Error::WriteDatabase {
                path: db_file.to_path_buf(),
                source: Box::new(source),
            })?;
    }
    Ok(())
}

/// Finalize the listing fetch progress bar and emit the completion log line.
fn finish_fetch_progress(
    fetch_pb: ProgressBar,
    indexed: usize,
    failed: usize,
    bytes_written: u64,
    fetch_elapsed: Duration,
) {
    let packages_per_sec = if fetch_elapsed.as_secs_f64() > 0.0 {
        let elapsed_secs = fetch_elapsed.as_secs_f64();
        let indexed_u64 = match u64::try_from(indexed) {
            Ok(idx) => idx,
            Err(_) => u64::MAX,
        };
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let rate = indexed_u64 as f64 / elapsed_secs;
        rate
    } else {
        0.0
    };
    info!(
        indexed,
        failed,
        bytes_written,
        packages_per_sec,
        ?fetch_elapsed,
        "listing fetch complete"
    );
    fetch_pb.finish_with_message(format!(
        "Fetched {indexed} listings ({failed} failed) in {:?}",
        fetch_elapsed
    ));
}

async fn write_listings(
    writer: &mut Writer,
    fetcher: &Fetcher,
    jobs: usize,
    package_input: mpsc::Receiver<listings::PackageEntry>,
    path_cache: Option<Arc<PathCache>>,
    filter_prefix: Vec<u8>,
    db_file: &Path,
    progress: &MultiProgress,
) -> Result<(usize, usize, Duration)> {
    // Flush raw packages to a v2 frame once the in-memory chunk reaches 256 MiB.
    const CHUNK_BYTES: u64 = 256 * 1024 * 1024;

    let fetch_pb = progress.add(ProgressBar::new_spinner());
    fetch_pb.set_message("Fetching listings...");
    let fetch_start = quanta::Instant::now();

    let mut listings = listings::fetch_listings(fetcher, jobs, package_input, path_cache)
        .await
        .map_err(|source| {
            Error::Io(std::io::Error::other(format!(
                "failed to start listing fetcher: {source}"
            )))
        })?;

    let mut indexed = 0usize;
    let mut failed = 0usize;
    let mut bytes_written = 0u64;

    while let Some(result) = listings.recv().await {
        match result {
            Ok((store_path, tree)) => {
                let before_len = writer.estimated_size();
                writer
                    .add(&store_path, &tree, &filter_prefix)
                    .map_err(|source| Error::WriteDatabase {
                        path: db_file.to_path_buf(),
                        source: Box::new(source),
                    })?;
                let after_len = writer.estimated_size();
                bytes_written += after_len.saturating_sub(before_len);
                indexed += 1;
                maybe_flush_chunk(writer, db_file, CHUNK_BYTES)?;
            }
            Err(err) => {
                warn!(error = %err, "closure fetch yielded an error; skipping");
                failed += 1;
            }
        }
        fetch_pb.set_message(format!("Indexed {indexed}, failed {failed}"));
        fetch_pb.inc(1);
    }
    let fetch_elapsed = fetch_start.elapsed();
    finish_fetch_progress(fetch_pb, indexed, failed, bytes_written, fetch_elapsed);

    Ok((indexed, failed, fetch_elapsed))
}

/// Build or update the nixdex index.
///
/// # Errors
///
/// Returns an error if the index build fails.
pub async fn update(options: &UpdateOptions) -> Result<()> {
    let builder = IndexBuilder::new(options.clone());
    builder.build().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_match_upstream_baseline() {
        let opts = UpdateOptions::default();
        assert_eq!(opts.jobs, 100);
        assert_eq!(opts.compression_level, 19);
        assert_eq!(opts.format_version, 2);
        assert_eq!(opts.nixpkgs, "<nixpkgs>");
        assert!(!opts.path_cache);
        assert!(opts.main_program);
    }

    /// Networked end-to-end smoke test against the public binary cache.
    /// Marked `ignore` so the default suite stays offline.
    #[tokio::test]
    #[ignore = "requires network + nix-eval-jobs"]
    async fn index_hello_from_nixpkgs() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Evaluate a tiny attrset rather than all of nixpkgs.
        let expr = r"{ hello = (import <nixpkgs> {}).hello; }";
        let opts = UpdateOptions {
            jobs: 4,
            database: dir.path().to_path_buf(),
            nixpkgs: expr.to_string(),
            system: None,
            compression_level: 3,
            format_version: 1,
            show_trace: false,
            filter_prefix: "/bin/".into(),
            path_cache: false,
            force: false,
            cache_key: None,
            path_cache_file: None,
            path_cache_ttl: None,
            main_program: true,
            extra_scopes: vec![],
        };
        IndexBuilder::new(opts).build().await.expect("build");
        assert!(dir.path().join("files").exists());
    }
}
