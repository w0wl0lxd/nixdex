//! Shared CLI logic for the `nixdex`, `nix-index`, and `nix-locate` binaries.

use std::sync::OnceLock;

pub mod daemon_client;
pub mod index;
pub mod locate;
pub mod tui;

/// Default index directory, as the string clap needs for `default_value`.
///
/// Every `--db` argument in this crate shares it, so the default cannot drift
/// between `nixdex`, `nixdex locate` and `nixdex tui`. The value is computed
/// once and leaked into a `OnceLock` because clap wants a `&'static str`.
///
/// A path that is not valid UTF-8 falls back to `/tmp/nixdex`: clap cannot take
/// a non-UTF-8 default, and `NIX_INDEX_DATABASE` overrides it either way.
pub fn default_db_dir() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            nixdex_core::nixdex_dir()
                .into_os_string()
                .into_string()
                .unwrap_or_else(|_| String::from("/tmp/nixdex"))
        })
        .as_str()
}
