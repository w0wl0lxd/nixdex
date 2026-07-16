//! XDG-compliant cache-directory helpers.

use std::path::PathBuf;

fn cache_base_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
}

/// Return the default directory used to store the `nix-index` / `nix-locate`
/// database and sidecars.
#[must_use]
pub fn nix_index_dir() -> PathBuf {
    cache_base_dir().join("nix-index")
}

/// Return the default directory used to store nixdex daemon caches.
#[must_use]
pub fn nixdex_dir() -> PathBuf {
    cache_base_dir().join("nixdex")
}
