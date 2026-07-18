//! Building a nixdex index from nixpkgs and the binary cache.

use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::database::{Writer, read_attrs_sidecar};
use crate::errors::{Error, Result};
use crate::hydra::Fetcher;
use crate::listings;
use crate::nixpkgs;
use crate::path_cache::PathCache;

/// Name of the package metadata sidecar written alongside `files`.
const PACKAGES_JSON: &str = "packages.json";

/// Context for the listing write operation.
struct ListingContext {
    db_file: PathBuf,
    writer: Writer,
    path_cache: Option<Arc<PathCache>>,
    cache_path: PathBuf,
    attrs_map: IndexMap<String, String>,
}

/// Context for the write_listings function.
struct WriteListingsContext<'a> {
    writer: &'a mut Writer,
    fetcher: &'a Fetcher,
    jobs: usize,
    path_cache: Option<Arc<PathCache>>,
    filter_prefix: &'a [u8],
    exclude_prefixes: Vec<Vec<u8>>,
    db_file: &'a Path,
    fetch_pb: ProgressBar,
    /// Maximum uncompressed bytes to buffer before flushing a v2 frame.
    chunk_size: u64,
    attrs_map: IndexMap<String, String>,
    no_closure: bool,
}

/// Options controlling an index build.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Number of concurrent HTTP requests.
    pub jobs: usize,
    /// Per-request HTTP timeout in seconds.
    pub timeout: u64,
    /// Maximum number of retries for transient HTTP failures.
    pub retries: u32,
    /// Directory where the index database is stored.
    pub database: PathBuf,
    /// nixpkgs path/expression (for example `<nixpkgs>`).
    pub nixpkgs: String,
    /// Optional system triple for evaluation.
    pub system: Option<String>,
    /// Optional `--select` expression passed to `nix-eval-jobs`.
    pub select: Option<String>,
    /// Whether to pass `--no-instantiate` to `nix-eval-jobs`.
    pub no_instantiate: bool,
    /// Whether to pass `--check-cache-status` to `nix-eval-jobs`.
    pub check_cache_status: bool,
    /// Zstandard compression level for the on-disk database.
    pub compression_level: i32,
    /// Maximum uncompressed bytes to buffer before flushing a v2 frame.
    pub chunk_size: u64,
    /// On-disk database format version (1 or 2).
    pub format_version: u64,
    /// Build the optional `redb` exact-path sidecar.
    pub enable_redb: bool,
    /// Pass `--show-trace` to Nix evaluation.
    pub show_trace: bool,
    /// Only index paths starting with this prefix.
    pub filter_prefix: String,
    /// Build a small, `/bin/`-filtered database (alias for `--filter-prefix /bin/`).
    pub small: bool,
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
    /// Disable nixpkgs overlays during evaluation.
    pub no_overlays: bool,
    /// Do not recurse into runtime references when fetching `.ls` listings.
    pub no_closure: bool,
    /// Extra attribute scopes to walk during evaluation.
    pub extra_scopes: Vec<String>,
    /// Only evaluate nixpkgs; do not fetch listings or write the files database.
    pub only_eval: bool,
    /// Base URL of the Nix binary cache to fetch listings from.
    pub cache_url: String,
    /// Only add paths that do not start with any of these prefixes.
    pub exclude_prefix: Vec<String>,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            jobs: 100,
            timeout: 30,
            retries: 4,
            database: PathBuf::from("/tmp/nix-index"),
            nixpkgs: String::from("<nixpkgs>"),
            system: None,
            select: None,
            no_instantiate: false,
            check_cache_status: true,
            compression_level: 22,
            chunk_size: 64 * 1024 * 1024,
            format_version: 2,
            enable_redb: false,
            show_trace: false,
            filter_prefix: String::new(),
            small: false,
            path_cache: false,
            force: false,
            cache_key: None,
            path_cache_file: None,
            path_cache_ttl: None,
            main_program: true,
            no_overlays: false,
            no_closure: false,
            extra_scopes: vec![
                String::from("haskellPackages"),
                String::from("rPackages"),
                String::from("coqPackages"),
                String::from("texlive.pkgs"),
            ],
            only_eval: false,
            cache_url: crate::CACHE_URL.to_string(),
            exclude_prefix: Vec::new(),
        }
    }
}

/// Orchestrates eval → fetch → write for a nixdex index.
#[derive(Debug, Default)]
pub struct IndexBuilder {
    options: UpdateOptions,
}

/// Handles for the concurrently-running evaluation and metadata writer tasks,
/// plus the package receiver consumed by the listing fetcher.
struct EvalStream {
    eval: tokio::task::JoinHandle<nixpkgs::Result<(usize, Duration)>>,
    meta: tokio::task::JoinHandle<Result<()>>,
    packages: mpsc::Receiver<listings::PackageEntry>,
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

    /// Spawn the package metadata writer that serializes [`nixpkgs::PackageMeta`]
    /// records into `<database>/packages.json`.
    fn spawn_meta_writer(
        &self,
        rx: mpsc::Receiver<nixpkgs::PackageMeta>,
    ) -> tokio::task::JoinHandle<Result<()>> {
        let path = self.options.database.join(PACKAGES_JSON);
        tokio::spawn(async move {
            let file = File::create(&path)
                .await
                .map_err(|source| Error::CreateFile {
                    path: path.clone(),
                    source: Box::new(source),
                })?;
            let mut writer = BufWriter::new(file);
            let mut rx = rx;
            while let Some(meta) = rx.recv().await {
                let line = sonic_rs::to_string(&meta)?;
                writer
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|source| Error::WriteFile {
                        path: path.clone(),
                        source: Box::new(source),
                    })?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|source| Error::WriteFile {
                        path: path.clone(),
                        source: Box::new(source),
                    })?;
            }
            writer.flush().await.map_err(|source| Error::WriteFile {
                path: path.clone(),
                source: Box::new(source),
            })?;
            Ok(())
        })
    }

    /// Spawn an async task that evaluates nixpkgs and streams
    /// [`PackageEntry`] values into a channel, updating `progress` per entry.
    fn spawn_package_eval_stream(&self, progress: ProgressBar) -> EvalStream {
        let opts = &self.options;
        let (pkg_tx, pkg_rx) = mpsc::channel(1024);
        let (meta_tx, meta_rx) = mpsc::channel(1024);
        let nixpkgs_expr = opts.nixpkgs.clone();
        let system = opts.system.clone();
        let extra_scopes = opts.extra_scopes.clone();
        let select = opts.select.clone();
        let show_trace = opts.show_trace;
        let main_program = opts.main_program;
        let no_instantiate = opts.no_instantiate;
        let check_cache_status = opts.check_cache_status;
        let no_overlays = opts.no_overlays;

        let meta_handle = self.spawn_meta_writer(meta_rx);
        let handle = tokio::spawn(async move {
            let start = Instant::now();
            let base = nixpkgs::EvalJobsOptions {
                nixpkgs: &nixpkgs_expr,
                system: system.as_deref(),
                select: select.as_deref(),
                no_instantiate,
                check_cache_status,
                show_trace,
                // Meta is always fetched so the `packages.json` sidecar has
                // descriptions and `mainProgram` values.
                meta: true,
                scope: None,
                no_overlays,
            };
            let count = nixpkgs::stream_package_entries(
                base,
                &extra_scopes,
                main_program,
                pkg_tx,
                meta_tx,
                &progress,
            )
            .await?;
            Ok((count, start.elapsed()))
        });

        EvalStream {
            eval: handle,
            meta: meta_handle,
            packages: pkg_rx,
        }
    }

    /// Prepare the output database directory, `paths.cache`, and `Writer`.
    fn prepare_database(&self) -> Result<ListingContext> {
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

        let attrs_map = self.load_attrs_sidecar();

        let db_file = opts.database.join("files");
        let writer = Writer::create_with_version(
            &db_file,
            opts.compression_level,
            opts.format_version,
            opts.enable_redb,
        )
        .map_err(|source| Error::CreateDatabase {
            path: db_file.clone(),
            source: Box::new(source),
        })?;

        Ok(ListingContext {
            db_file,
            writer,
            path_cache,
            cache_path,
            attrs_map,
        })
    }

    /// Load the attrs sidecar from the previous build if path_cache is enabled.
    #[allow(clippy::cognitive_complexity)]
    fn load_attrs_sidecar(&self) -> IndexMap<String, String> {
        let opts = &self.options;
        if !opts.path_cache {
            return IndexMap::new();
        }

        match read_attrs_sidecar(&opts.database) {
            Ok(Some(attrs)) => {
                let mut map = IndexMap::with_capacity(attrs.len());
                for (attr, output, hash) in attrs {
                    map.insert(format!("{}.{}", attr, output), hash);
                }
                info!(
                    count = map.len(),
                    "loaded attrs sidecar for incremental build"
                );
                map
            }
            Ok(None) => {
                info!("attrs sidecar not found; full rebuild");
                IndexMap::new()
            }
            Err(err) => {
                warn!(error = %err, "failed to read attrs sidecar; full rebuild");
                IndexMap::new()
            }
        }
    }

    /// Build a fresh binary-cache fetcher.
    fn new_fetcher(&self) -> Result<Fetcher> {
        use std::time::Duration;
        Fetcher::builder(&self.options.cache_url)
            .timeout(Duration::from_secs(self.options.timeout))
            .max_attempts(self.options.retries.saturating_add(1))
            .build()
            .map_err(|err| {
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

    /// Await the package metadata writer task and log any failure.
    #[allow(clippy::cognitive_complexity)]
    async fn await_meta(handle: tokio::task::JoinHandle<Result<()>>) {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(error = %err, "failed to write package metadata sidecar"),
            Err(err) => warn!(error = %err, "meta writer task panicked"),
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

    /// Run an eval-only pass and write the package metadata sidecar.
    async fn build_eval_only(&self) -> Result<()> {
        let opts = &self.options;
        std::fs::create_dir_all(&opts.database).map_err(|source| Error::CreateDatabaseDir {
            path: opts.database.clone(),
            source,
        })?;
        let eval_start = quanta::Instant::now();
        let eval_pb = eval_spinner();
        eval_pb.set_message("Evaluating nixpkgs...");

        let mut stream = self.spawn_package_eval_stream(eval_pb);
        while stream.packages.recv().await.is_some() {}
        let (eval_count, eval_elapsed) = Self::await_eval(stream.eval).await?;
        Self::await_meta(stream.meta).await;
        let total_elapsed = eval_start.elapsed();
        info!(
            eval_count,
            ?eval_elapsed,
            ?total_elapsed,
            db = %opts.database.display(),
            "eval-only run complete"
        );
        Ok(())
    }

    /// Run the index build: evaluate packages, fetch `.ls` trees, write NIXI DB.
    ///
    /// # Errors
    ///
    /// Returns an error if the database directory cannot be created, evaluation
    /// fails hard, or the writer cannot be finalized.
    pub async fn build(&self) -> Result<()> {
        let opts = &self.options;

        if opts.only_eval {
            return self.build_eval_only().await;
        }

        let mut ctx = self.prepare_database()?;
        let progress = MultiProgress::new();
        let eval_pb = progress.add(eval_spinner());
        eval_pb.set_message("Evaluating nixpkgs...");
        let fetch_pb = fetch_bar(&progress);
        fetch_pb.set_message("Fetching listings...");

        let stream = self.spawn_package_eval_stream(eval_pb.clone());
        // The fetch phase total is not known at evaluation time because the
        // closure fetcher discovers additional runtime references as it
        // processes each root, so leave the fetch progress bar indeterminate.
        let eval_handle = tokio::spawn(Self::await_eval(stream.eval));

        let fetcher = self.new_fetcher()?;
        let filter_prefix = if opts.small && opts.filter_prefix.is_empty() {
            b"/bin/".to_vec()
        } else {
            opts.filter_prefix.as_bytes().to_vec()
        };
        let exclude_prefixes = opts
            .exclude_prefix
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        let (indexed, failed, fetch_elapsed) = match write_listings(
            WriteListingsContext {
                writer: &mut ctx.writer,
                fetcher: &fetcher,
                jobs: opts.jobs.max(1),
                path_cache: ctx.path_cache.clone(),
                filter_prefix: &filter_prefix,
                exclude_prefixes,
                db_file: &ctx.db_file,
                fetch_pb: fetch_pb.clone(),
                chunk_size: opts.chunk_size,
                attrs_map: ctx.attrs_map,
                no_closure: opts.no_closure,
            },
            stream.packages,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => {
                eval_handle.abort();
                return Err(err);
            }
        };

        let (eval_count, eval_elapsed) = match eval_handle.await {
            Ok(Ok(value)) => value,
            Ok(Err(err)) => return Err(err),
            Err(err) => {
                return Err(Error::Io(std::io::Error::other(format!(
                    "eval task panicked: {err}"
                ))));
            }
        };
        Self::await_meta(stream.meta).await;
        eval_pb.finish_with_message(format!(
            "Evaluated {eval_count} package(s) in {eval_elapsed:?}"
        ));

        Self::maybe_save_path_cache(opts.path_cache, ctx.path_cache.as_ref(), &ctx.cache_path);

        let size = ctx.writer.finish().map_err(|source| Error::WriteDatabase {
            path: ctx.db_file.clone(),
            source: Box::new(source),
        })?;

        let cached = ctx
            .path_cache
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
            db = %ctx.db_file.display(),
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

/// Create a spinner progress bar for the nixpkgs evaluation phase.
fn eval_spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} Evaluating nixpkgs... {pos} entries {per_sec} {elapsed_precise}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb
}

/// Create a spinner progress bar for the listing fetch phase and attach it to `multi`.
/// The total amount of work is not known ahead of time because closure
/// traversal discovers additional store paths as it runs.
fn fetch_bar(multi: &MultiProgress) -> ProgressBar {
    let pb = multi.add(ProgressBar::new_spinner());
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} Fetching listings... {pos} entries {per_sec} {elapsed_precise}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb
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
    ctx: WriteListingsContext<'_>,
    package_input: mpsc::Receiver<listings::PackageEntry>,
) -> Result<(usize, usize, Duration)> {
    let fetch_pb = ctx.fetch_pb;
    fetch_pb.set_message("Fetching listings...");
    let fetch_start = quanta::Instant::now();

    let mut listings = listings::fetch_listings(
        ctx.fetcher,
        ctx.jobs,
        package_input,
        ctx.path_cache,
        ctx.filter_prefix,
        ctx.attrs_map,
        ctx.no_closure,
    )
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
                let before_len = ctx.writer.estimated_size();
                let exclude_slices: Vec<&[u8]> = ctx
                    .exclude_prefixes
                    .iter()
                    .map(std::vec::Vec::as_slice)
                    .collect();
                ctx.writer
                    .add_excluding(&store_path, &tree, ctx.filter_prefix, &exclude_slices)
                    .map_err(|source| Error::WriteDatabase {
                        path: ctx.db_file.to_path_buf(),
                        source: Box::new(source),
                    })?;
                let after_len = ctx.writer.estimated_size();
                bytes_written += after_len.saturating_sub(before_len);
                indexed += 1;
                maybe_flush_chunk(ctx.writer, ctx.db_file, ctx.chunk_size)?;
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
        assert_eq!(opts.compression_level, 22);
        assert_eq!(opts.chunk_size, 64 * 1024 * 1024);
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
        let nixpkgs_file = dir.path().join("small.nix");
        std::fs::write(&nixpkgs_file, r"{ hello = (import <nixpkgs> {}).hello; }")
            .expect("write fixture");
        let opts = UpdateOptions {
            jobs: 4,
            timeout: 30,
            retries: 4,
            database: dir.path().to_path_buf(),
            nixpkgs: nixpkgs_file.to_string_lossy().into_owned(),
            system: None,
            select: None,
            no_instantiate: false,
            check_cache_status: true,
            compression_level: 3,
            chunk_size: 4 * 1024 * 1024,
            format_version: 1,
            enable_redb: false,
            show_trace: false,
            filter_prefix: "/bin/".into(),
            small: false,
            path_cache: false,
            force: false,
            cache_key: None,
            path_cache_file: None,
            path_cache_ttl: None,
            main_program: true,
            no_overlays: false,
            no_closure: false,
            extra_scopes: vec![],
            only_eval: false,
            cache_url: crate::CACHE_URL.to_string(),
            exclude_prefix: Vec::new(),
        };
        IndexBuilder::new(opts).build().await.expect("build");
        assert!(dir.path().join("files").exists());
    }
}
