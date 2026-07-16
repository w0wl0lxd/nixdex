//! Background daemon support for keeping the nixdex index up to date.

use std::sync::Arc;

use crate::basename_index::BasenameIndex;
use crate::database::Reader;
use crate::errors::Result;
use crate::prebuilt::{self, PrebuiltConfig};
use indexmap::IndexSet;

#[cfg(feature = "daemon")]
use axum::{Router, extract::State, routing::get};
#[cfg(feature = "daemon")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "daemon")]
struct IndexState {
    basename: Arc<std::sync::RwLock<Option<Arc<BasenameIndex>>>>,
    reader: Arc<std::sync::RwLock<Option<Arc<Reader>>>>,
}

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
    let index_state = Arc::new(IndexState {
        basename: Arc::new(std::sync::RwLock::new(None)),
        reader: Arc::new(std::sync::RwLock::new(None)),
    });

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
    if let Ok(mut state) = index_state.basename.write() {
        *state = Some(Arc::new(index));
    }

    load_reader(cache_dir, index_state);
    tracing::info!("prebuilt index refreshed and loaded");
}

#[cfg(feature = "daemon")]
fn load_reader(cache_dir: &std::path::Path, index_state: &IndexState) {
    let current = cache_dir.join("current");
    let files_path = current.join("files");
    let Ok(reader) = Reader::open(&files_path) else {
        tracing::warn!("failed to open database reader for prebuilt index");
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
        .route("/locate", get(locate_handler))
        .route("/nix-locate", get(nix_locate_handler))
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
#[cfg(feature = "daemon")]
#[allow(clippy::too_many_lines)]
async fn nix_locate_handler(
    State(index_state): State<Arc<IndexState>>,
    axum::extract::Query(params): axum::extract::Query<NixLocateParams>,
) -> axum::Json<NixLocateResponse> {
    let reader = {
        let Ok(guard) = index_state.reader.read() else {
            return axum::Json(NixLocateResponse {
                matches: Vec::new(),
            });
        };
        guard.as_ref().map(Arc::clone)
    };

    let basename_index = {
        let Ok(guard) = index_state.basename.read() else {
            return axum::Json(NixLocateResponse {
                matches: Vec::new(),
            });
        };
        guard.as_ref().map(Arc::clone)
    };

    let (Some(reader), Some(basename_index)) = (reader, basename_index) else {
        return axum::Json(NixLocateResponse {
            matches: Vec::new(),
        });
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

    // Compile regex and package regex if provided.
    let Ok(path_re) = regex::bytes::Regex::new(&pattern) else {
        return axum::Json(NixLocateResponse {
            matches: Vec::new(),
        });
    };

    let package_re = params
        .package
        .as_ref()
        .and_then(|p| regex::bytes::Regex::new(p).ok());

    // Try exact_basename fast path via BasenameIndex.
    let exact_basename_ordinals =
        if !params.regex && params.whole_name && !params.pattern.is_empty() {
            let base = crate::basename_index::basename_of(params.pattern.as_bytes());
            if params.pattern.contains('/') && !base.is_empty() {
                match basename_index.lookup_basename_ordinals(base) {
                    Ok(ordinals) if !ordinals.is_empty() => Some(ordinals.into_iter().collect()),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        };

    // CPU-bound search: use spawn_blocking to avoid blocking the tokio runtime.
    let reader_clone = Arc::clone(&reader);
    let hash = params.hash.clone();
    let Ok(Ok(matches)) = tokio::task::spawn_blocking(move || {
        reader_clone
            .search_entries(
                &path_re,
                package_re.as_ref(),
                hash.as_deref(),
                None,
                exact_basename_ordinals.as_ref(),
            )
            .map_err(|e| format!("{:?}", e))
    })
    .await
    else {
        return axum::Json(NixLocateResponse {
            matches: Vec::new(),
        });
    };

    // Convert results to JSON response.
    let json_matches: Vec<NixLocateMatch> = matches
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
    let json_matches = if params.minimal {
        // In minimal mode, we only need unique attr.output pairs.
        let mut seen = IndexSet::new();
        json_matches
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
            .collect()
    } else {
        json_matches
    };

    axum::Json(NixLocateResponse {
        matches: json_matches,
    })
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
}

#[cfg(feature = "daemon")]
#[derive(Serialize)]
struct NixLocateResponse {
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
