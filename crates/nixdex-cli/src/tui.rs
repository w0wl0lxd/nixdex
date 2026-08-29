pub use nixdex_tui::run_tui;

use clap::Parser;
use std::path::PathBuf;

/// Options for `nixdex tui`.
#[derive(Debug, Parser)]
pub struct TuiOpts {
    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = default_db_dir(), env = "NIX_INDEX_DATABASE")]
    pub database: PathBuf,
}

fn default_db_dir() -> &'static str {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            nixdex_core::nixdex_dir()
                .into_os_string()
                .into_string()
                .unwrap_or_else(|_| String::from("/tmp/nixdex"))
        })
        .as_str()
}
