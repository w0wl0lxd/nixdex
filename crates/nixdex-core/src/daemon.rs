//! Background daemon support for keeping the nixdex index up to date.

use std::time::Duration;

use crate::errors::Result;
use crate::index::UpdateOptions;
use crate::update_index;

/// Configuration for a one-shot or looping daemon run.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Options passed through to each index refresh.
    pub update_options: UpdateOptions,
    /// Interval between refresh attempts.
    pub interval: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            update_options: UpdateOptions::default(),
            interval: Duration::from_hours(24),
        }
    }
}

/// Run the daemon loop, refreshing the index at `config.interval` until a
/// termination signal is received.
///
/// Listens for `SIGTERM` and `SIGINT`. A refresh that fails is logged and the
/// loop continues.
///
/// # Errors
///
/// Returns an error only when signal setup fails.
pub async fn run(config: &DaemonConfig) -> Result<()> {
    let mut interval = tokio::time::interval(config.interval);
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    tracing::info!(
        database = %config.update_options.database.display(),
        interval_secs = config.interval.as_secs(),
        "nixdex daemon started"
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                tracing::info!("refreshing index");
                match update_index(&config.update_options).await {
                    Ok(()) => tracing::info!("refresh complete"),
                    Err(err) => tracing::error!(error = %err, "refresh failed"),
                }
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT, shutting down");
                break;
            }
        }
    }

    Ok(())
}
