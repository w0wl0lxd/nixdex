//! Background daemon support for keeping the nixdex index up to date.

use std::path::PathBuf;
use std::time::Duration;

use crate::errors::{Error, Result};

/// Configuration for a one-shot or looping daemon run.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Directory that holds the index database.
    pub database: PathBuf,
    /// Interval between refresh attempts.
    pub interval: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            database: PathBuf::from("/tmp/nix-index"),
            interval: Duration::from_hours(24),
        }
    }
}

/// Run a single daemon cycle (log, optional sleep, exit).
///
/// Scaffold behaviour: log the configured path/interval and return successfully
/// without building an index.
///
/// # Errors
///
/// Returns an error only when configuration is invalid.
pub async fn run(config: &DaemonConfig) -> Result<()> {
    tracing::info!(
        database = %config.database.display(),
        interval_secs = config.interval.as_secs(),
        "nixdex daemon cycle (scaffold — no refresh performed)"
    );

    if config.interval.is_zero() {
        return Err(Error::NotImplemented(
            "daemon interval of zero is reserved for future multi-cycle mode",
        ));
    }

    // Single short sleep so the binary exercises async runtime without hanging.
    tokio::time::sleep(Duration::from_millis(10)).await;
    Ok(())
}
