//! Background daemon support for keeping the nixdex index up to date.

use std::sync::Arc;

use crate::basename_index::BasenameIndex;
use crate::errors::Result;
use crate::prebuilt::{self, PrebuiltConfig};

#[cfg(feature = "daemon")]
use axum::{
    extract::State,
    routing::get,
    Router,
};
#[cfg(feature = "daemon")]
use serde::{Deserialize, Serialize};

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
    let cache_dir = config.prebuilt.cache_dir.clone();
    let index_state = Arc::new(std::sync::RwLock::new(None::<BasenameIndex>));

    let http_handle = start_http_server(&config.http_addr, Arc::clone(&index_state));

    log_daemon_start(config, &cache_dir);

    let mut interval = tokio::time::interval(config.prebuilt.refresh_interval);
    run_daemon_loop(&config.prebuilt, &cache_dir, &index_state, &mut interval, http_handle).await
}

#[cfg(feature = "daemon")]
fn start_http_server(
    addr: &str,
    index_state: Arc<std::sync::RwLock<Option<BasenameIndex>>>,
) -> tokio::task::JoinHandle<Result<()>> {
    let http_addr = addr.to_string();
    tokio::spawn(async move {
        run_http_server(&http_addr, index_state).await
    })
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
    index_state: &Arc<std::sync::RwLock<Option<BasenameIndex>>>,
    interval: &mut tokio::time::Interval,
    http_handle: tokio::task::JoinHandle<Result<()>>,
) -> Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                handle_refresh_tick(prebuilt_config, cache_dir, index_state).await;
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                http_handle.abort();
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT, shutting down");
                http_handle.abort();
                break;
            }
        }
    }

    Ok(())
}

#[cfg(feature = "daemon")]
#[allow(clippy::cognitive_complexity)]
async fn handle_refresh_tick(
    prebuilt_config: &PrebuiltConfig,
    cache_dir: &std::path::Path,
    index_state: &Arc<std::sync::RwLock<Option<BasenameIndex>>>,
) {
    tracing::info!("checking for prebuilt index update");
    match refresh_prebuilt(prebuilt_config, cache_dir).await {
        Ok(()) => {
            if let Ok(index) = prebuilt::open_current_basename_index(cache_dir)
                && let Ok(mut state) = index_state.write()
            {
                *state = Some(index);
                tracing::info!("prebuilt index refreshed and loaded");
            }
        }
        Err(err) => tracing::error!(error = %err, "prebuilt refresh failed"),
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
async fn run_http_server(
    addr: &str,
    index_state: Arc<std::sync::RwLock<Option<BasenameIndex>>>,
) -> Result<()> {
    let app = Router::new()
        .route("/locate", get(locate_handler))
        .with_state(index_state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;

    tracing::info!(addr, "HTTP server listening");

    axum::serve(listener, app)
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;

    Ok(())
}

/// HTTP handler for `/locate?basename=<name>`.
#[cfg(feature = "daemon")]
async fn locate_handler(
    State(index_state): State<Arc<std::sync::RwLock<Option<BasenameIndex>>>>,
    axum::extract::Query(params): axum::extract::Query<LocateParams>,
) -> axum::Json<LocateResponse> {
    let Ok(index) = index_state.read() else {
        return axum::Json(LocateResponse { packages: Vec::new() });
    };

    let Some(index) = index.as_ref() else {
        return axum::Json(LocateResponse { packages: Vec::new() });
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
