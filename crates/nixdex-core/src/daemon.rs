//! Background daemon support for keeping the nixdex index up to date.

use std::path::PathBuf;
use std::sync::Arc;

use crate::basename_index::BasenameIndex;
use crate::database::Reader;
use crate::errors::Result;
use crate::prebuilt::{self, PrebuiltConfig};
use indexmap::IndexSet;

#[cfg(feature = "daemon")]
use axum::{
    Router,
    extract::State,
    http::header::CONTENT_TYPE,
    response::IntoResponse,
    routing::{get, post},
};
#[cfg(feature = "daemon")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "daemon")]
use std::str::FromStr;
#[cfg(feature = "daemon")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "daemon")]
use std::time::Instant;

#[cfg(feature = "daemon")]
struct IndexState {
    basename: Arc<std::sync::RwLock<Option<Arc<BasenameIndex>>>>,
    reader: Arc<std::sync::RwLock<Option<Arc<Reader>>>>,
    package_db: Arc<std::sync::RwLock<Option<Arc<crate::package_search::SearchDb>>>>,
    /// Directory that currently holds the loaded `files` database and sidecars.
    database_dir: Arc<std::sync::RwLock<Option<PathBuf>>>,
    start_time: Instant,
    requests_total: AtomicU64,
    /// Channel used by HTTP `/reload` to request an immediate index refresh.
    reload: tokio::sync::mpsc::UnboundedSender<()>,
}

/// Configuration for a prebuilt-index daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Prebuilt index configuration.
    pub prebuilt: PrebuiltConfig,
    /// HTTP server listen address.
    pub http_addr: String,
    /// Optional local database directory to serve instead of downloading a prebuilt index.
    pub local_database: Option<PathBuf>,
    /// How often to reload the local database when running in local mode.
    pub local_refresh_interval: std::time::Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            prebuilt: PrebuiltConfig::default(),
            http_addr: "127.0.0.1:3750".to_string(),
            local_database: None,
            local_refresh_interval: std::time::Duration::from_secs(3600),
        }
    }
}

/// Action emitted by the daemon signal handler.
#[cfg(feature = "daemon")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalAction {
    /// Shut down the daemon.
    Shutdown,
    /// Force an immediate index reload.
    Reload,
}

/// Run the daemon loop, polling for prebuilt index updates and serving HTTP requests.
///
/// Listens for `SIGTERM`, `SIGINT`, and `SIGHUP`. A `SIGHUP` or `POST /reload`
/// triggers an immediate refresh. A refresh that fails is logged and the loop
/// continues.
///
/// # Errors
///
/// Returns an error when signal setup or HTTP server binding fails.
#[cfg(feature = "daemon")]
#[allow(clippy::cognitive_complexity)]
pub async fn run(config: &DaemonConfig) -> Result<()> {
    let (reload_tx, mut reload_rx) = tokio::sync::mpsc::unbounded_channel();
    let index_state = Arc::new(IndexState {
        basename: Arc::new(std::sync::RwLock::new(None)),
        reader: Arc::new(std::sync::RwLock::new(None)),
        package_db: Arc::new(std::sync::RwLock::new(None)),
        database_dir: Arc::new(std::sync::RwLock::new(None)),
        start_time: Instant::now(),
        requests_total: AtomicU64::new(0),
        reload: reload_tx,
    });

    let http_handle = start_http_server(&config.http_addr, Arc::clone(&index_state));

    if let Some(local) = &config.local_database {
        if config.local_refresh_interval.is_zero() {
            return Err(crate::Error::Io(std::io::Error::other(
                "local refresh interval must be non-zero",
            )));
        }

        let mut local_interval = tokio::time::interval(config.local_refresh_interval);
        local_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            load_and_store_index(local, &index_state);
            log_daemon_start_local(config, local);
            tokio::select! {
                _ = local_interval.tick() => {
                    tracing::info!("local refresh interval elapsed; reloading index");
                }
                result = wait_signal() => {
                    match result {
                        Ok(SignalAction::Reload) => {
                            tracing::info!("received reload signal; reloading local index");
                        }
                        Ok(SignalAction::Shutdown) => {
                            http_handle.abort();
                            return Ok(());
                        }
                        Err(err) => {
                            tracing::error!(error = %err, "signal handler failed");
                            http_handle.abort();
                            return Err(err);
                        }
                    }
                }
                Some(()) = reload_rx.recv() => {
                    tracing::info!("received /reload request; reloading local index");
                }
            }
        }
    }

    if config.prebuilt.refresh_interval.is_zero() {
        return Err(crate::Error::Io(std::io::Error::other(
            "daemon refresh interval must be non-zero",
        )));
    }

    let cache_dir = config.prebuilt.cache_dir.clone();
    log_daemon_start(config, &cache_dir);

    let mut interval = tokio::time::interval(config.prebuilt.refresh_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    run_daemon_loop(
        &config.prebuilt,
        &cache_dir,
        &index_state,
        &mut interval,
        &mut reload_rx,
        http_handle,
    )
    .await
}

#[cfg(feature = "daemon")]
fn log_daemon_start_local(config: &DaemonConfig, database: &std::path::Path) {
    tracing::info!(
        database = %database.display(),
        http_addr = %config.http_addr,
        "nixdex daemon started in local mode"
    );
}

#[cfg(feature = "daemon")]
fn start_http_server(
    addr: &str,
    index_state: Arc<IndexState>,
) -> tokio::task::JoinHandle<Result<()>> {
    let http_addr = addr.to_string();
    tokio::spawn(async move { run_http_server(&http_addr, index_state).await })
}

#[cfg(feature = "daemon")]
fn log_daemon_start(config: &DaemonConfig, cache_dir: &std::path::Path) {
    tracing::info!(
        cache_dir = %cache_dir.display(),
        interval_secs = config.prebuilt.refresh_interval.as_secs(),
        http_addr = %config.http_addr,
        "nixdex daemon started"
    );
}

#[cfg(feature = "daemon")]
#[allow(clippy::cognitive_complexity)]
async fn run_daemon_loop(
    prebuilt_config: &PrebuiltConfig,
    cache_dir: &std::path::Path,
    index_state: &IndexState,
    interval: &mut tokio::time::Interval,
    reload_rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    http_handle: tokio::task::JoinHandle<Result<()>>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = interval.tick() => {
                handle_refresh_tick(prebuilt_config, cache_dir, index_state).await;
            }
            result = wait_signal() => {
                match result {
                    Ok(SignalAction::Reload) => {
                        tracing::info!("received reload signal; refreshing prebuilt index");
                        handle_refresh_tick(prebuilt_config, cache_dir, index_state).await;
                    }
                    Ok(SignalAction::Shutdown) => {
                        http_handle.abort();
                        break;
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "signal handler failed");
                        http_handle.abort();
                        return Err(err);
                    }
                }
            }
            Some(()) = reload_rx.recv() => {
                tracing::info!("received /reload request; refreshing prebuilt index");
                handle_refresh_tick(prebuilt_config, cache_dir, index_state).await;
            }
        }
    }

    Ok(())
}

#[cfg(feature = "daemon")]
async fn wait_signal() -> Result<SignalAction> {
    #[cfg(unix)]
    {
        wait_unix_signal().await
    }
    #[cfg(not(unix))]
    {
        wait_ctrl_c().await
    }
}

// The 15-point cognitive-complexity budget is too small for a `tokio::select!`
// over three signal handlers; splitting this further would hurt readability.
#[allow(clippy::cognitive_complexity)]
#[cfg(unix)]
async fn wait_unix_signal() -> Result<SignalAction> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM, shutting down");
            Ok(SignalAction::Shutdown)
        }
        _ = sigint.recv() => {
            tracing::info!("received SIGINT, shutting down");
            Ok(SignalAction::Shutdown)
        }
        _ = sighup.recv() => {
            tracing::info!("received SIGHUP, reloading index");
            Ok(SignalAction::Reload)
        }
    }
}

#[cfg(not(unix))]
async fn wait_ctrl_c() -> Result<SignalAction> {
    tokio::signal::ctrl_c().await.map_err(crate::Error::Io)?;
    tracing::info!("received Ctrl+C, shutting down");
    Ok(SignalAction::Shutdown)
}

#[cfg(feature = "daemon")]
// Refreshed below the threshold after extracting `load_and_store_index`; remaining
// complexity comes from the required `match` on the fallible refresh result.
#[allow(clippy::cognitive_complexity)]
async fn handle_refresh_tick(
    prebuilt_config: &PrebuiltConfig,
    cache_dir: &std::path::Path,
    index_state: &IndexState,
) {
    tracing::info!("checking for prebuilt index update");
    match refresh_prebuilt(prebuilt_config, cache_dir).await {
        Ok(()) => load_and_store_index(cache_dir, index_state),
        Err(err) => tracing::error!(error = %err, "prebuilt refresh failed"),
    }
}

#[cfg(feature = "daemon")]
fn load_and_store_index(cache_dir: &std::path::Path, index_state: &IndexState) {
    let index_dir = if cache_dir.join("files").exists() {
        cache_dir.to_path_buf()
    } else {
        cache_dir.join("current")
    };

    if let Ok(mut state) = index_state.database_dir.write() {
        *state = Some(index_dir.clone());
    }

    if !ensure_sidecars(&index_dir) {
        return;
    }

    let Ok(index) = crate::basename_index::BasenameIndex::open(&index_dir) else {
        return;
    };
    if let Ok(mut state) = index_state.basename.write() {
        *state = Some(Arc::new(index));
    }

    load_reader(&index_dir, index_state);
    load_package_db(&index_dir, index_state);
    tracing::info!(index_dir = %index_dir.display(), "index loaded");
}

#[cfg(feature = "daemon")]
fn ensure_sidecars(index_dir: &std::path::Path) -> bool {
    let files_path = index_dir.join("files");
    let packages_json = index_dir.join("packages.json");
    if files_path.is_file()
        && !packages_json.is_file()
        && let Err(err) = crate::database::generate_sidecars(&files_path)
    {
        tracing::warn!(
            error = %err,
            path = %files_path.display(),
            "failed to generate sidecars for prebuilt index"
        );
        return false;
    }
    true
}

#[cfg(feature = "daemon")]
fn load_package_db(index_dir: &std::path::Path, index_state: &IndexState) {
    let packages_json = index_dir.join("packages.json");
    if !packages_json.exists() {
        return;
    }
    match crate::package_search::SearchDb::open(&packages_json) {
        Ok(db) => {
            if let Ok(mut state) = index_state.package_db.write() {
                *state = Some(Arc::new(db));
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, path = %packages_json.display(), "failed to load package metadata sidecar");
        }
    }
}

#[cfg(feature = "daemon")]
fn load_reader(index_dir: &std::path::Path, index_state: &IndexState) {
    let files_path = index_dir.join("files");
    let Ok(reader) = Reader::open(&files_path) else {
        tracing::warn!(path = %files_path.display(), "failed to open database reader");
        return;
    };
    if let Ok(mut state) = index_state.reader.write() {
        *state = Some(Arc::new(reader));
    }
}

/// Refresh the prebuilt index if a new version is available.
#[cfg(feature = "daemon")]
async fn refresh_prebuilt(config: &PrebuiltConfig, cache_dir: &std::path::Path) -> Result<()> {
    let current = prebuilt::current_dir(cache_dir)?;
    let remote_etag = prebuilt::check_update(config).await?;

    if should_download_index(current.as_ref(), remote_etag.as_ref()) {
        download_and_update(config, cache_dir).await?;
    } else {
        tracing::info!("prebuilt index up to date");
    }

    Ok(())
}

#[cfg(feature = "daemon")]
fn should_download_index(
    current: Option<&std::path::PathBuf>,
    remote_etag: Option<&String>,
) -> bool {
    match (current, remote_etag) {
        (None, _) => true,
        (Some(curr_dir), Some(remote)) => {
            let current_etag = match curr_dir.file_name().and_then(|n| n.to_str()) {
                Some(e) => e,
                None => "",
            };
            current_etag != remote
        }
        _ => false,
    }
}

#[cfg(feature = "daemon")]
// This is a minimal async wrapper around two fallible operations; further
// decomposition would create artificial seams without improving clarity.
#[allow(clippy::cognitive_complexity)]
async fn download_and_update(config: &PrebuiltConfig, cache_dir: &std::path::Path) -> Result<()> {
    tracing::info!("downloading new prebuilt index");
    let target_dir = prebuilt::download_and_validate(config).await?;
    prebuilt::update_current_symlink(cache_dir, &target_dir)?;
    tracing::info!("prebuilt index downloaded and symlink updated");
    Ok(())
}

/// Run the HTTP server for basename lookups.
#[cfg(feature = "daemon")]
async fn run_http_server(addr: &str, index_state: Arc<IndexState>) -> Result<()> {
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/reload", post(reload_handler))
        .route("/locate", get(locate_handler))
        .route("/nix-locate", get(nix_locate_handler))
        .route("/search", get(search_handler))
        .with_state(index_state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(crate::Error::from)?;

    tracing::info!(addr, "HTTP server listening");

    axum::serve(listener, app)
        .await
        .map_err(crate::Error::from)?;

    Ok(())
}

/// HTTP handler for `/locate?basename=<name>`.
#[cfg(feature = "daemon")]
async fn locate_handler(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::Query(params): axum::extract::Query<LocateParams>,
) -> axum::Json<LocateResponse> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);
    let index = {
        let Ok(guard) = index_state.basename.read() else {
            return axum::Json(LocateResponse {
                packages: Vec::new(),
            });
        };
        guard.as_ref().map(Arc::clone)
    };

    let Some(index) = index else {
        return axum::Json(LocateResponse {
            packages: Vec::new(),
        });
    };

    let packages = match index.lookup_basename(params.basename.as_bytes()) {
        Ok(pkgs) => pkgs.into_iter().map(std::string::String::from).collect(),
        Err(_) => Vec::new(),
    };

    axum::Json(LocateResponse { packages })
}

#[cfg(feature = "daemon")]
#[derive(Deserialize)]
struct LocateParams {
    basename: String,
}

#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct LocateResponse {
    packages: Vec<String>,
}

/// HTTP handler for `/nix-locate` with nix-index-compatible parameters.
///
/// Supports the same filtering/sorting options as the CLI `nix-locate`,
/// including `type`, `min_size`, `max_size`, `exclude_fhs`, and `sort`.
#[cfg(feature = "daemon")]
#[allow(clippy::too_many_lines)]
async fn nix_locate_handler(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::Query(params): axum::extract::Query<NixLocateParams>,
) -> std::result::Result<axum::Json<NixLocateResponse>, axum::http::StatusCode> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);

    let database_dir = {
        let guard = index_state
            .database_dir
            .read()
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        guard
            .as_ref()
            .cloned()
            .ok_or(axum::http::StatusCode::SERVICE_UNAVAILABLE)?
    };

    // Build the path regex with anchors based on at_root/whole_name.
    let start_anchor = if params.at_root { "^" } else { "" };
    let end_anchor = if params.whole_name { "$" } else { "" };
    let pattern_body = if params.regex {
        params.pattern.clone()
    } else {
        regex::escape(&params.pattern)
    };
    let pattern = format!("{start_anchor}{pattern_body}{end_anchor}");

    // Determine whether the secondary indexes can answer this query exactly.
    let exact_basename = if !params.regex && params.whole_name && !params.pattern.is_empty() {
        let base = crate::basename_index::basename_of(params.pattern.as_bytes());
        if params.pattern.contains('/') && !base.is_empty() {
            Some(String::from_utf8_lossy(base).into_owned())
        } else {
            None
        }
    } else {
        None
    };

    let (exact_path, path_prefix) = if !params.regex && params.at_root && !params.pattern.is_empty()
    {
        let pattern_bytes = params.pattern.as_bytes();
        if pattern_bytes.starts_with(b"/") {
            if params.whole_name {
                (Some(params.pattern.clone()), None)
            } else {
                (None, Some(params.pattern.clone()))
            }
        } else if params.pattern.contains('/') {
            (None, Some(format!("/{}", params.pattern)))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    let package_pattern = params.package.clone();

    // CPU-bound search: use spawn_blocking to avoid blocking the tokio runtime.
    let search_task = tokio::task::spawn_blocking(move || {
        let file_type: Vec<crate::FileType> = if params.file_type.is_empty() {
            crate::ALL_FILE_TYPES.to_vec()
        } else {
            let mut types = Vec::with_capacity(params.file_type.len());
            for s in &params.file_type {
                match crate::FileType::from_str(s) {
                    Ok(ft) => types.push(ft),
                    Err(_) => return Err("bad_request: invalid file type".to_string()),
                }
            }
            types
        };

        let sort = match &params.sort {
            Some(s) => crate::database::SearchSort::from_str(s)
                .map_err(|_| "bad_request: invalid sort order".to_string())?,
            None => crate::database::SearchSort::None,
        };

        let options = crate::database::SearchOptions {
            database: database_dir,
            pattern,
            hash: params.hash,
            package_pattern,
            exact_basename,
            exact_path,
            path_prefix,
            file_type: &file_type,
            mode: crate::database::SearchMode::Full {
                color: false,
                group: false,
                only_toplevel: false,
            },
            json: false,
            limit: None,
            count: false,
            sort,
            min_size: params.min_size,
            max_size: params.max_size,
            exclude_fhs: params.exclude_fhs,
        };

        crate::search_database_results(&options).map_err(|e| format!("search error: {e:?}"))
    });

    let results = match search_task.await {
        Ok(Ok(results)) => results,
        Ok(Err(err)) if err.starts_with("bad_request:") => {
            return Err(axum::http::StatusCode::BAD_REQUEST);
        }
        _ => return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
    };

    // Convert results to JSON response.
    let mut json_matches: Vec<NixLocateMatch> = results
        .into_iter()
        .map(|(store_path, entry)| {
            let node = match entry.node {
                crate::files::FileNode::Regular { executable, size } => {
                    let typ = if executable { "x" } else { "r" };
                    NixLocateNode::Regular {
                        r#type: typ.to_string(),
                        size,
                    }
                }
                crate::files::FileNode::Directory { size, .. } => NixLocateNode::Directory {
                    r#type: "d".to_string(),
                    size,
                },
                crate::files::FileNode::Symlink { target } => NixLocateNode::Symlink {
                    r#type: "s".to_string(),
                    target: String::from_utf8_lossy(&target).to_string(),
                },
            };
            NixLocateMatch {
                attr: store_path.origin().attr.clone(),
                output: store_path.origin().output.clone(),
                name: store_path.name().to_string(),
                hash: store_path.hash().to_string(),
                path: String::from_utf8_lossy(&entry.path).to_string(),
                node,
            }
        })
        .collect();

    // Filter to minimal output if requested.
    if params.minimal {
        let mut seen = IndexSet::new();
        json_matches = json_matches
            .into_iter()
            .filter_map(|m| {
                let key = format!("{}.{}", m.attr, m.output);
                if seen.insert(key) {
                    Some(NixLocateMatch {
                        attr: m.attr,
                        output: m.output,
                        name: String::new(),
                        hash: String::new(),
                        path: String::new(),
                        node: NixLocateNode::Regular {
                            r#type: String::new(),
                            size: 0,
                        },
                    })
                } else {
                    None
                }
            })
            .collect();
    }

    if params.count {
        return Ok(axum::Json(NixLocateResponse {
            count: Some(json_matches.len()),
            matches: Vec::new(),
        }));
    }

    if let Some(limit) = params.limit {
        json_matches.truncate(limit);
    }

    Ok(axum::Json(NixLocateResponse {
        count: None,
        matches: json_matches,
    }))
}

#[cfg(feature = "daemon")]
#[derive(Deserialize)]
struct NixLocateParams {
    pattern: String,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    at_root: bool,
    #[serde(default)]
    whole_name: bool,
    #[serde(default)]
    minimal: bool,
    #[serde(default, rename = "type")]
    file_type: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    count: bool,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    min_size: Option<u64>,
    #[serde(default)]
    max_size: Option<u64>,
    #[serde(default)]
    exclude_fhs: bool,
}

#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct NixLocateResponse {
    count: Option<usize>,
    matches: Vec<NixLocateMatch>,
}

#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct NixLocateMatch {
    attr: String,
    output: String,
    name: String,
    hash: String,
    path: String,
    #[serde(flatten)]
    node: NixLocateNode,
}

#[cfg(feature = "daemon")]
#[derive(Serialize)]
#[serde(untagged)]
enum NixLocateNode {
    Regular { r#type: String, size: u64 },
    Directory { r#type: String, size: u64 },
    Symlink { r#type: String, target: String },
}

/// Health-check response.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    index_loaded: bool,
    package_db_loaded: bool,
    package_count: Option<usize>,
    version: &'static str,
    uptime_seconds: u64,
}

/// HTTP handler for `/health`.
#[cfg(feature = "daemon")]
async fn health_handler(State(index_state): State<Arc<IndexState>>) -> axum::Json<HealthResponse> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);
    let index_loaded = matches!(index_state.basename.read(), Ok(g) if g.is_some());
    let package_db_loaded = matches!(index_state.package_db.read(), Ok(g) if g.is_some());
    let package_count = index_state
        .reader
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|r| r.package_count()))
        .flatten();

    axum::Json(HealthResponse {
        status: "ok",
        index_loaded,
        package_db_loaded,
        package_count,
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: index_state.start_time.elapsed().as_secs(),
    })
}

/// Reload response.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct ReloadResponse {
    reloaded: bool,
}

/// HTTP handler for `POST /reload`.
#[cfg(feature = "daemon")]
async fn reload_handler(State(index_state): State<Arc<IndexState>>) -> axum::Json<ReloadResponse> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);
    let reloaded = index_state.reload.send(()).is_ok();
    axum::Json(ReloadResponse { reloaded })
}

/// Prometheus-style `/metrics` handler.
#[cfg(feature = "daemon")]
async fn metrics_handler(State(index_state): State<Arc<IndexState>>) -> impl IntoResponse {
    let total = index_state.requests_total.load(Ordering::Relaxed);
    let index_loaded = u64::from(matches!(index_state.basename.read(), Ok(g) if g.is_some()));
    let package_db_loaded =
        u64::from(matches!(index_state.package_db.read(), Ok(g) if g.is_some()));
    let uptime = index_state.start_time.elapsed().as_secs();
    let version = env!("CARGO_PKG_VERSION");

    let body = format!(
        "# HELP nixdex_requests_total Total number of HTTP requests served by nixdex-daemon.\n\
         # TYPE nixdex_requests_total counter\n\
         nixdex_requests_total {total}\n\
         # HELP nixdex_index_loaded Whether the file index is loaded (1 = yes, 0 = no).\n\
         # TYPE nixdex_index_loaded gauge\n\
         nixdex_index_loaded {index_loaded}\n\
         # HELP nixdex_package_db_loaded Whether the package metadata sidecar is loaded.\n\
         # TYPE nixdex_package_db_loaded gauge\n\
         nixdex_package_db_loaded {package_db_loaded}\n\
         # HELP nixdex_uptime_seconds Daemon uptime in seconds.\n\
         # TYPE nixdex_uptime_seconds gauge\n\
         nixdex_uptime_seconds {uptime}\n\
         # HELP nixdex_version_info Build version.\n\
         # TYPE nixdex_version_info gauge\n\
         nixdex_version_info{{version=\"{version}\"}} 1\n"
    );

    ([(CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

/// Parameters for `/search`.
#[cfg(feature = "daemon")]
#[derive(Deserialize)]
struct SearchParams {
    pattern: String,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    exact: bool,
    #[serde(default)]
    fuzzy: bool,
    #[serde(default)]
    field: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    count: bool,
    #[serde(default)]
    name_only: bool,
}

/// HTTP handler for `/search`.
#[cfg(feature = "daemon")]
async fn search_handler(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::Query(params): axum::extract::Query<SearchParams>,
) -> axum::Json<SearchResponse> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);
    let db = {
        let Ok(guard) = index_state.package_db.read() else {
            return axum::Json(SearchResponse {
                count: None,
                names: None,
                results: Vec::new(),
            });
        };
        guard.as_ref().map(Arc::clone)
    };

    let Some(db) = db else {
        return axum::Json(SearchResponse {
            count: None,
            names: None,
            results: Vec::new(),
        });
    };

    let Ok(field) = crate::package_search::SearchField::from_str(&params.field) else {
        return axum::Json(SearchResponse {
            count: None,
            names: None,
            results: Vec::new(),
        });
    };

    let matched = if params.fuzzy {
        match db.search_fuzzy(&params.pattern, field, params.case_sensitive, params.limit) {
            Ok(m) => m,
            Err(_) => {
                return axum::Json(SearchResponse {
                    count: None,
                    names: None,
                    results: Vec::new(),
                });
            }
        }
    } else {
        match db.search(
            &params.pattern,
            params.regex,
            field,
            params.case_sensitive,
            params.exact,
            params.limit,
        ) {
            Ok(m) => m,
            Err(_) => {
                return axum::Json(SearchResponse {
                    count: None,
                    names: None,
                    results: Vec::new(),
                });
            }
        }
    };

    if params.count {
        return axum::Json(SearchResponse {
            count: Some(matched.len()),
            names: None,
            results: Vec::new(),
        });
    }

    if params.name_only {
        let names = matched
            .into_iter()
            .map(|record| record.attr.clone())
            .collect();
        return axum::Json(SearchResponse {
            count: None,
            names: Some(names),
            results: Vec::new(),
        });
    }

    let results = matched.into_iter().cloned().collect();

    axum::Json(SearchResponse {
        count: None,
        names: None,
        results,
    })
}

/// Response for `/search`.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct SearchResponse {
    count: Option<usize>,
    names: Option<Vec<String>>,
    results: Vec<crate::PackageMeta>,
}
