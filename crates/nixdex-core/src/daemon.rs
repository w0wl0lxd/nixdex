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
/// Listens for `SIGTERM` and `SIGINT` on Unix, and `Ctrl+C` elsewhere. A
/// refresh that fails is logged and the loop continues.
///
/// # Errors
///
/// Returns an error when the interval is zero or signal setup fails.
pub async fn run(config: &DaemonConfig) -> Result<()> {
    if config.interval.is_zero() {
        return Err(crate::errors::Error::Io(std::io::Error::other(
            "daemon interval must be non-zero",
        )));
    }

    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
            _ = wait_signal() => {
                break;
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
async fn wait_signal() -> Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    tokio::select! {
        _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => tracing::info!("received SIGINT, shutting down"),
    }

    Ok(())
}

#[cfg(not(unix))]
async fn wait_signal() -> Result<()> {
    tokio::signal::ctrl_c().await?;
    tracing::info!("received Ctrl+C, shutting down");
    Ok(())
}
