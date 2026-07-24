//! Background daemon support for keeping the nixdex index up to date.

use std::path::PathBuf;
use std::sync::Arc;

use crate::basename_index::BasenameIndex;
use crate::database::Reader;
use crate::errors::Result;
use crate::prebuilt::{self, PrebuiltConfig};
use indexmap::IndexSet;

/// Maximum length (in bytes) of an HTTP query pattern.
const MAX_PATTERN_BYTES: usize = 1024;

/// Maximum number of results returned by daemon endpoints.
const MAX_RESULT_LIMIT: usize = 10_000;

#[cfg(feature = "daemon")]
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    response::Response,
    routing::{get, post},
};
#[cfg(feature = "daemon")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "daemon")]
use std::net::SocketAddr;
#[cfg(feature = "daemon")]
use std::str::FromStr;
#[cfg(feature = "daemon")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "daemon")]
use std::time::Instant;

#[cfg(feature = "daemon")]
#[derive(Clone)]
struct IndexSnapshot {
    /// Directory that currently holds the loaded `files` database and sidecars.
    database_dir: PathBuf,
    /// Loaded basename index.
    basename: Arc<BasenameIndex>,
    /// Loaded path index.
    path_index: Arc<crate::path_index::PathIndex>,
    /// Loaded command-provider index.
    command_index: Arc<crate::command_index::CommandIndex>,
    /// Loaded database reader.
    reader: Arc<Reader>,
    /// Loaded package metadata database, if present.
    package_db: Option<Arc<crate::package_search::SearchDb>>,
}

#[cfg(feature = "daemon")]
struct IndexState {
    /// Currently loaded index; replaced atomically so requests never see a partially loaded state.
    index: Arc<std::sync::RwLock<Option<IndexSnapshot>>>,
    start_time: Instant,
    requests_total: AtomicU64,
    /// Bearer token required for the `POST /reload` admin endpoint.
    admin_token: Option<String>,
    /// Bounded one-slot channel used by HTTP `/reload` to request an immediate index refresh.
    /// Pending requests cannot grow beyond one; new requests replace pending ones.
    reload: tokio::sync::mpsc::Sender<()>,
}

#[cfg(feature = "daemon")]
fn read_snapshot(index_state: &IndexState) -> Option<IndexSnapshot> {
    match index_state.index.read() {
        Ok(guard) => guard.as_ref().cloned(),
        Err(err) => {
            tracing::warn!(error = %err, "index lock poisoned");
            None
        }
    }
}

/// Index loading strategy for the daemon.
///
/// Both modes keep the (mmap-backed) database reader resident for the lifetime
/// of the process. The difference is whether the heavy `entry`/`ngram` secondary
/// indexes are built eagerly at load (`Resident`, the default) or deferred until
/// they are first needed (`Lru`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexCacheMode {
    /// Build all secondary indexes eagerly at load.
    #[default]
    Resident,
    /// Defer the heavy entry/ngram indexes; build them lazily on first use.
    Lru,
}

impl IndexCacheMode {
    /// Whether the heavy `entry`/`ngram` secondary indexes should be generated.
    #[must_use]
    pub fn include_heavy(self) -> bool {
        matches!(self, Self::Resident)
    }
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
    /// Bearer token required for the `POST /reload` admin endpoint.
    /// If unset, `/reload` is only accepted from loopback addresses.
    pub admin_token: Option<String>,
    /// How aggressively secondary indexes are loaded into memory.
    pub index_cache_mode: IndexCacheMode,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            prebuilt: PrebuiltConfig::default(),
            http_addr: "127.0.0.1:3750".to_string(),
            local_database: None,
            local_refresh_interval: std::time::Duration::from_secs(3600),
            admin_token: None,
            index_cache_mode: IndexCacheMode::Resident,
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
    let (reload_tx, mut reload_rx) = tokio::sync::mpsc::channel(1);
    let index_state = Arc::new(IndexState {
        index: Arc::new(std::sync::RwLock::new(None)),
        start_time: Instant::now(),
        requests_total: AtomicU64::new(0),
        admin_token: config.admin_token.clone(),
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
    reload_rx: &mut tokio::sync::mpsc::Receiver<()>,
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
#[cfg(all(feature = "daemon", unix))]
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

#[cfg(all(feature = "daemon", not(unix)))]
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
#[allow(clippy::cognitive_complexity)]
fn load_and_store_index(cache_dir: &std::path::Path, index_state: &IndexState) {
    let index_dir = if cache_dir.join("files").exists() {
        cache_dir.to_path_buf()
    } else {
        cache_dir.join("current")
    };

    if !ensure_sidecars(&index_dir) {
        return;
    }

    let basename = match crate::basename_index::BasenameIndex::open(&index_dir) {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(error = %err, path = %index_dir.display(), "failed to load basename index");
            return;
        }
    };

    let path_index = match crate::path_index::PathIndex::open(&index_dir) {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(error = %err, path = %index_dir.display(), "failed to load path index");
            return;
        }
    };

    let command_index = match crate::command_index::CommandIndex::open(&index_dir) {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(error = %err, path = %index_dir.display(), "failed to load command index");
            return;
        }
    };

    let files_path = index_dir.join("files");
    let reader = match Reader::open(&files_path) {
        Ok(reader) => reader,
        Err(err) => {
            tracing::warn!(error = %err, path = %files_path.display(), "failed to open database reader");
            return;
        }
    };

    // Pre-fault mmap pages so the first query doesn't pay page-fault latency.
    reader.prefault();

    let package_db = {
        let packages_json = index_dir.join("packages.json");
        if packages_json.exists() {
            match crate::package_search::SearchDb::open(&packages_json) {
                Ok(db) => Some(Arc::new(db)),
                Err(err) => {
                    tracing::warn!(error = %err, path = %packages_json.display(), "failed to load package metadata sidecar");
                    None
                }
            }
        } else {
            None
        }
    };

    let snapshot = IndexSnapshot {
        database_dir: index_dir.clone(),
        basename: Arc::new(basename),
        path_index: Arc::new(path_index),
        command_index: Arc::new(command_index),
        reader: Arc::new(reader),
        package_db,
    };

    if let Ok(mut state) = index_state.index.write() {
        *state = Some(snapshot);
    }

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

/// Authenticate `POST /reload` requests.
///
/// If `admin_token` is configured, the caller must present it as an
/// `Authorization: Bearer <token>` header (compared in constant time).
/// If no token is configured, the endpoint is restricted to loopback addresses.
#[cfg(feature = "daemon")]
async fn admin_auth_middleware(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    fn unauthorized() -> Response {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Bearer"),
        );
        (StatusCode::UNAUTHORIZED, headers).into_response()
    }

    if let Some(expected) = &index_state.admin_token {
        let presented = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "));

        let Some(presented) = presented else {
            return unauthorized();
        };

        if constant_time_bearer_eq(presented, expected) {
            next.run(request).await
        } else {
            unauthorized()
        }
    } else if addr.ip().is_loopback() {
        next.run(request).await
    } else {
        unauthorized()
    }
}

/// Compare a presented bearer token to the expected value in constant time.
#[cfg(feature = "daemon")]
fn constant_time_bearer_eq(presented: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;

    let presented_bytes = presented.as_bytes();
    let expected_bytes = expected.as_bytes();
    let n = expected_bytes.len();

    // Zero-pad the presented token to the expected length so the same comparison
    // path runs regardless of input length, then fold in the length equality bit.
    let mut padded = vec![0u8; n];
    for (i, b) in presented_bytes.iter().take(n).enumerate() {
        if let Some(slot) = padded.get_mut(i) {
            *slot = *b;
        }
    }

    let bytes_match = padded.as_slice().ct_eq(expected_bytes).unwrap_u8();
    let len_match = u8::from(presented_bytes.len() == n);
    (bytes_match & len_match) == 1
}

/// Run the HTTP server for basename lookups.
#[cfg(feature = "daemon")]
async fn run_http_server(addr: &str, index_state: Arc<IndexState>) -> Result<()> {
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/version", get(version_handler))
        .route("/metrics", get(metrics_handler))
        .route(
            "/reload",
            post(reload_handler).route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&index_state),
                admin_auth_middleware,
            )),
        )
        .route("/locate", get(locate_handler))
        .route("/nix-locate", get(nix_locate_handler))
        .route("/search", get(search_handler))
        .route("/command", get(command_handler))
        .with_state(index_state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(crate::Error::from)?;

    tracing::info!(addr, "HTTP server listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
    let Some(snapshot) = read_snapshot(&index_state) else {
        return axum::Json(LocateResponse {
            packages: Vec::new(),
        });
    };

    let packages = match snapshot
        .basename
        .lookup_basename(params.basename.as_bytes())
    {
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
) -> std::result::Result<
    axum::Json<NixLocateResponse>,
    (axum::http::StatusCode, axum::Json<ErrorResponse>),
> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);

    if params.pattern.len() > MAX_PATTERN_BYTES {
        return Err(json_error(
            axum::http::StatusCode::BAD_REQUEST,
            format!("pattern exceeds maximum length of {MAX_PATTERN_BYTES} bytes"),
        ));
    }

    let limit = match params.limit {
        Some(limit) => limit,
        None => MAX_RESULT_LIMIT,
    };
    if limit > MAX_RESULT_LIMIT {
        return Err(json_error(
            axum::http::StatusCode::BAD_REQUEST,
            format!("limit must be at most {MAX_RESULT_LIMIT}"),
        ));
    }

    let (database_dir, resident_path, resident_basename, package_db) =
        match read_snapshot(&index_state) {
            Some(snapshot) => (
                snapshot.database_dir,
                Some(std::sync::Arc::clone(&snapshot.path_index)),
                Some(std::sync::Arc::clone(&snapshot.basename)),
                snapshot.package_db,
            ),
            None => {
                return Err(json_error(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "no database loaded",
                ));
            }
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
                only_toplevel: !params.all,
            },
            json: false,
            limit: Some(limit),
            count: false,
            sort,
            min_size: params.min_size,
            max_size: params.max_size,
            exclude_fhs: params.exclude_fhs,
            null_output: params.null_output,
            literal_pattern: (!params.regex
                && !params.at_root
                && !params.whole_name
                && !params.pattern.is_empty())
            .then(|| params.pattern.clone()),
        };

        let resident = crate::database::ResidentIndexes {
            path_index: resident_path.as_deref(),
            basename_index: resident_basename.as_deref(),
        };

        crate::search_database_results(&options, Some(resident)).map_err(|e| match e {
            crate::Error::Parse(_) => format!("bad_request: {e}"),
            _ => format!("search error: {e:?}"),
        })
    });

    let results = match search_task.await {
        Ok(Ok(results)) => results,
        Ok(Err(err)) if err.starts_with("bad_request:") => {
            return Err(json_error(axum::http::StatusCode::BAD_REQUEST, err));
        }
        _ => {
            return Err(json_error(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "search failed",
            ));
        }
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
                name: Some(store_path.name().to_string()),
                hash: Some(store_path.hash().to_string()),
                path: Some(String::from_utf8_lossy(&entry.path).to_string()),
                node: Some(node),
                description: package_db
                    .as_deref()
                    .and_then(|db| db.lookup_attr(store_path.origin().attr.as_str()))
                    .and_then(|m| m.description.clone()),
                license: package_db
                    .as_deref()
                    .and_then(|db| db.lookup_attr(store_path.origin().attr.as_str()))
                    .and_then(|m| m.license.clone()),
                homepage: package_db
                    .as_deref()
                    .and_then(|db| db.lookup_attr(store_path.origin().attr.as_str()))
                    .and_then(|m| m.homepage.clone()),
                maintainers: package_db
                    .as_deref()
                    .and_then(|db| db.lookup_attr(store_path.origin().attr.as_str()))
                    .and_then(|m| m.maintainers.clone()),
                platforms: package_db
                    .as_deref()
                    .and_then(|db| db.lookup_attr(store_path.origin().attr.as_str()))
                    .and_then(|m| m.platforms.clone()),
                main_program: package_db
                    .as_deref()
                    .and_then(|db| db.lookup_attr(store_path.origin().attr.as_str()))
                    .and_then(|m| m.main_program.clone()),
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
                        name: None,
                        hash: None,
                        path: None,
                        node: None,
                        description: None,
                        license: None,
                        homepage: None,
                        maintainers: None,
                        platforms: None,
                        main_program: None,
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

    json_matches.truncate(limit);

    Ok(axum::Json(NixLocateResponse {
        count: None,
        matches: json_matches,
    }))
}

#[cfg(feature = "daemon")]
fn deserialize_file_types<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct FileTypeVisitor;

    impl<'de> serde::de::Visitor<'de> for FileTypeVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a comma-separated string or a sequence of file types")
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect())
        }

        fn visit_seq<S>(self, seq: S) -> std::result::Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            serde::Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(FileTypeVisitor)
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
    #[serde(default, rename = "type", deserialize_with = "deserialize_file_types")]
    file_type: Vec<String>,
    #[serde(default)]
    all: bool,
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
    #[serde(default)]
    null_output: bool,
}

#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[cfg(feature = "daemon")]
fn json_error(
    status: axum::http::StatusCode,
    message: impl Into<String>,
) -> (axum::http::StatusCode, axum::Json<ErrorResponse>) {
    (
        status,
        axum::Json(ErrorResponse {
            error: message.into(),
        }),
    )
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
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<NixLocateNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    maintainers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platforms: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    main_program: Option<String>,
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
    let (index_loaded, package_db_loaded, package_count) = match read_snapshot(&index_state) {
        Some(snapshot) => (
            true,
            snapshot.package_db.is_some(),
            snapshot.reader.package_count(),
        ),
        None => (false, false, None),
    };

    axum::Json(HealthResponse {
        status: "ok",
        index_loaded,
        package_db_loaded,
        package_count,
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: index_state.start_time.elapsed().as_secs(),
    })
}

/// Readiness response.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct ReadyResponse {
    ready: bool,
    index_loaded: bool,
    package_db_loaded: bool,
}

/// HTTP handler for `/ready`.
#[cfg(feature = "daemon")]
async fn ready_handler(
    State(index_state): State<Arc<IndexState>>,
) -> (axum::http::StatusCode, axum::Json<ReadyResponse>) {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);
    let (index_loaded, package_db_loaded) = match read_snapshot(&index_state) {
        Some(snapshot) => (true, snapshot.package_db.is_some()),
        None => (false, false),
    };
    let ready = index_loaded && package_db_loaded;

    let status = if ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        axum::Json(ReadyResponse {
            ready,
            index_loaded,
            package_db_loaded,
        }),
    )
}

/// Version response.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct VersionResponse {
    version: &'static str,
}

/// HTTP handler for `/version`.
#[cfg(feature = "daemon")]
async fn version_handler(
    State(index_state): State<Arc<IndexState>>,
) -> axum::Json<VersionResponse> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);
    axum::Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
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
    let reloaded = index_state.reload.try_send(()).is_ok();
    axum::Json(ReloadResponse { reloaded })
}

/// Prometheus-style `/metrics` handler.
#[cfg(feature = "daemon")]
async fn metrics_handler(State(index_state): State<Arc<IndexState>>) -> impl IntoResponse {
    let total = index_state.requests_total.load(Ordering::Relaxed);
    let (index_loaded, package_db_loaded) = match read_snapshot(&index_state) {
        Some(snapshot) => (u64::from(true), u64::from(snapshot.package_db.is_some())),
        None => (0, 0),
    };
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

    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
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
    #[serde(default, rename = "sort")]
    sort: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    count: bool,
    #[serde(default)]
    name_only: bool,
}

/// Parse the `field` and `sort` query parameters for `/search`.
#[cfg(feature = "daemon")]
fn parse_search_query(
    params: &SearchParams,
) -> std::result::Result<
    (
        crate::package_search::SearchField,
        crate::package_search::SearchSort,
    ),
    (axum::http::StatusCode, axum::Json<ErrorResponse>),
> {
    let field = crate::package_search::SearchField::from_str(&params.field)
        .map_err(|err| json_error(axum::http::StatusCode::BAD_REQUEST, err.to_string()))?;
    let sort = crate::package_search::SearchSort::from_str(&params.sort)
        .map_err(|err| json_error(axum::http::StatusCode::BAD_REQUEST, err.to_string()))?;
    Ok((field, sort))
}

/// HTTP handler for `/search`.
#[cfg(feature = "daemon")]
async fn search_handler(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::Query(params): axum::extract::Query<SearchParams>,
) -> std::result::Result<
    axum::Json<SearchResponse>,
    (axum::http::StatusCode, axum::Json<ErrorResponse>),
> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);

    if params.pattern.len() > MAX_PATTERN_BYTES {
        return Err(json_error(
            axum::http::StatusCode::BAD_REQUEST,
            format!("pattern exceeds maximum length of {MAX_PATTERN_BYTES} bytes"),
        ));
    }

    let limit = match params.limit {
        Some(limit) => limit,
        None => MAX_RESULT_LIMIT,
    };
    if limit > MAX_RESULT_LIMIT {
        return Err(json_error(
            axum::http::StatusCode::BAD_REQUEST,
            format!("limit must be at most {MAX_RESULT_LIMIT}"),
        ));
    }

    let db = match read_snapshot(&index_state) {
        Some(snapshot) => snapshot.package_db,
        None => {
            return Err(json_error(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "no package database loaded",
            ));
        }
    };

    let Some(db) = db else {
        return Err(json_error(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no package database loaded",
        ));
    };

    let (field, sort) = parse_search_query(&params)?;

    let matched = if params.fuzzy {
        db.search_fuzzy(
            &params.pattern,
            field,
            params.case_sensitive,
            sort,
            Some(limit),
        )
        .map_err(|err| json_error(axum::http::StatusCode::BAD_REQUEST, err.to_string()))?
    } else {
        db.search(
            &params.pattern,
            params.regex,
            field,
            params.case_sensitive,
            params.exact,
            sort,
            Some(limit),
        )
        .map_err(|err| json_error(axum::http::StatusCode::BAD_REQUEST, err.to_string()))?
    };

    if params.count {
        return Ok(axum::Json(SearchResponse {
            count: Some(matched.len()),
            names: None,
            results: Vec::new(),
        }));
    }

    if params.name_only {
        let names = matched
            .into_iter()
            .map(|record| record.attr.clone())
            .collect();
        return Ok(axum::Json(SearchResponse {
            count: None,
            names: Some(names),
            results: Vec::new(),
        }));
    }

    let results = matched.into_iter().cloned().collect();

    Ok(axum::Json(SearchResponse {
        count: None,
        names: None,
        results,
    }))
}

/// Response for `/search`.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct SearchResponse {
    count: Option<usize>,
    names: Option<Vec<String>>,
    results: Vec<crate::PackageMeta>,
}

/// Query parameters for `GET /command`.
#[cfg(feature = "daemon")]
#[derive(serde::Deserialize)]
struct CommandParams {
    /// Command name to look up (e.g. `git`).
    name: String,
}

/// A single package that provides a command.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct CommandProviderResponse {
    attr: String,
    output: String,
    toplevel: bool,
}

/// Response for `GET /command`.
#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct CommandResponse {
    command: String,
    providers: Vec<CommandProviderResponse>,
}

/// HTTP handler for `GET /command`.
///
/// Resolves the (resident) command-provider index in well under a millisecond;
/// it never touches the `files` blob or the `redb` reader.
#[cfg(feature = "daemon")]
async fn command_handler(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::Query(params): axum::extract::Query<CommandParams>,
) -> std::result::Result<
    axum::Json<CommandResponse>,
    (axum::http::StatusCode, axum::Json<ErrorResponse>),
> {
    index_state.requests_total.fetch_add(1, Ordering::Relaxed);

    if params.name.is_empty() {
        return Err(json_error(
            axum::http::StatusCode::BAD_REQUEST,
            "missing 'name' parameter",
        ));
    }

    let name = params.name;
    let Some(snapshot) = read_snapshot(&index_state) else {
        return Err(json_error(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no index loaded",
        ));
    };

    let providers = snapshot
        .command_index
        .lookup_command(name.as_bytes())
        .map_err(|err| {
            json_error(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            )
        })?;

    let providers = providers
        .into_iter()
        .map(|p| CommandProviderResponse {
            attr: p.attr,
            output: p.output,
            toplevel: p.toplevel,
        })
        .collect();

    Ok(axum::Json(CommandResponse {
        command: name,
        providers,
    }))
}

#[cfg(test)]
#[cfg(feature = "daemon")]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicU64;

    fn empty_state() -> Arc<IndexState> {
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(1);
        Arc::new(IndexState {
            index: Arc::new(std::sync::RwLock::new(None)),
            start_time: Instant::now(),
            requests_total: AtomicU64::new(0),
            admin_token: None,
            reload: reload_tx,
        })
    }

    fn state_with_token(token: Option<&str>) -> Arc<IndexState> {
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(1);
        Arc::new(IndexState {
            index: Arc::new(std::sync::RwLock::new(None)),
            start_time: Instant::now(),
            requests_total: AtomicU64::new(0),
            admin_token: token.map(String::from),
            reload: reload_tx,
        })
    }

    fn reload_app(state: Arc<IndexState>, addr: SocketAddr) -> Router {
        Router::new()
            .route(
                "/reload",
                post(reload_handler).route_layer(axum::middleware::from_fn_with_state(
                    Arc::clone(&state),
                    admin_auth_middleware,
                )),
            )
            .layer(axum::extract::connect_info::MockConnectInfo(addr))
            .with_state(state)
    }

    #[tokio::test]
    async fn version_handler_returns_version() {
        let state = empty_state();
        let response = version_handler(axum::extract::State(state)).await;
        assert_eq!(response.0.version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn ready_handler_returns_503_when_unloaded() {
        let state = empty_state();
        let (status, response) = ready_handler(axum::extract::State(state)).await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.0.ready);
        assert!(!response.0.index_loaded);
        assert!(!response.0.package_db_loaded);
    }

    #[tokio::test]
    async fn reload_rejects_missing_or_wrong_token() {
        let state = state_with_token(Some("secret"));
        let mut app = reload_app(state, SocketAddr::from(([127, 0, 0, 1], 1234)));

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/reload")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::<axum::extract::Request>::oneshot(&mut app, request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/reload")
            .header(axum::http::header::AUTHORIZATION, "Bearer wrong")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::<axum::extract::Request>::oneshot(&mut app, request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/reload")
            .header(axum::http::header::AUTHORIZATION, "Bearer secret")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::<axum::extract::Request>::oneshot(&mut app, request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn reload_requires_loopback_without_token() {
        let state = state_with_token(None);

        let mut app = reload_app(state.clone(), SocketAddr::from(([127, 0, 0, 1], 1234)));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/reload")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::<axum::extract::Request>::oneshot(&mut app, request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let mut app = reload_app(state, SocketAddr::from(([192, 168, 1, 1], 1234)));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/reload")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::<axum::extract::Request>::oneshot(&mut app, request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn constant_time_bearer_eq_is_strict() {
        assert!(constant_time_bearer_eq("secret", "secret"));
        assert!(!constant_time_bearer_eq("wrong", "secret"));
        assert!(!constant_time_bearer_eq("sec", "secret"));
        assert!(!constant_time_bearer_eq("secret-long", "secret"));
        assert!(!constant_time_bearer_eq("", "secret"));
    }
}
