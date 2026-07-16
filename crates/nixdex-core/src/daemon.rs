//! Background daemon support for keeping the nixdex index up to date.

use std::sync::Arc;

use crate::basename_index::BasenameIndex;
use crate::errors::Result;
use crate::prebuilt::{self, PrebuiltConfig};

#[cfg(feature = "daemon")]
use axum::{Router, extract::State, routing::get};
#[cfg(feature = "daemon")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "daemon")]
type IndexState = Arc<std::sync::RwLock<Option<Arc<BasenameIndex>>>>;

/// Configuration for a prebuilt-index daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Prebuilt index configuration.
    pub prebuilt: PrebuiltConfig,
    /// HTTP server listen address.
    pub http_addr: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            prebuilt: PrebuiltConfig::default(),
            http_addr: "127.0.0.1:3750".to_string(),
        }
    }
}

/// Run the daemon loop, polling for prebuilt index updates and serving HTTP requests.
///
/// Listens for `SIGTERM` and `SIGINT`. A refresh that fails is logged and the
/// loop continues.
///
/// # Errors
///
/// Returns an error when signal setup or HTTP server binding fails.
#[cfg(feature = "daemon")]
pub async fn run(config: &DaemonConfig) -> Result<()> {
    if config.prebuilt.refresh_interval.is_zero() {
        return Err(crate::Error::Io(std::io::Error::other(
            "daemon refresh interval must be non-zero",
        )));
    }

    let cache_dir = config.prebuilt.cache_dir.clone();
    let index_state: IndexState = Arc::new(std::sync::RwLock::new(None));

    let http_handle = start_http_server(&config.http_addr, Arc::clone(&index_state));

    log_daemon_start(config, &cache_dir);

    let mut interval = tokio::time::interval(config.prebuilt.refresh_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    run_daemon_loop(
        &config.prebuilt,
        &cache_dir,
        &index_state,
        &mut interval,
        http_handle,
    )
    .await
}

#[cfg(feature = "daemon")]
fn start_http_server(addr: &str, index_state: IndexState) -> tokio::task::JoinHandle<Result<()>> {
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
async fn run_daemon_loop(
    prebuilt_config: &PrebuiltConfig,
    cache_dir: &std::path::Path,
    index_state: &IndexState,
    interval: &mut tokio::time::Interval,
    http_handle: tokio::task::JoinHandle<Result<()>>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = interval.tick() => {
                handle_refresh_tick(prebuilt_config, cache_dir, index_state).await;
            }
            result = wait_signal() => {
                if let Err(err) = result {
                    tracing::error!(error = %err, "signal handler failed");
                }
                http_handle.abort();
                break;
            }
        }
    }

    Ok(())
}

#[cfg(feature = "daemon")]
async fn wait_signal() -> Result<()> {
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
// over two signal handlers; splitting this further would hurt readability.
#[allow(clippy::cognitive_complexity)]
#[cfg(unix)]
async fn wait_unix_signal() -> Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => tracing::info!("received SIGINT, shutting down"),
    }
    Ok(())
}

#[cfg(not(unix))]
async fn wait_ctrl_c() -> Result<()> {
    tokio::signal::ctrl_c().await.map_err(crate::Error::Io)?;
    tracing::info!("received Ctrl+C, shutting down");
    Ok(())
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
    let Ok(index) = prebuilt::open_current_basename_index(cache_dir) else {
        return;
    };
    if let Ok(mut state) = index_state.write() {
        *state = Some(Arc::new(index));
        tracing::info!("prebuilt index refreshed and loaded");
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
async fn run_http_server(addr: &str, index_state: IndexState) -> Result<()> {
    let app = Router::new()
        .route("/locate", get(locate_handler))
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
    State(index_state): State<IndexState>,
    axum::extract::Query(params): axum::extract::Query<LocateParams>,
) -> axum::Json<LocateResponse> {
    let index = {
        let Ok(guard) = index_state.read() else {
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
