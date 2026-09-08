pub use nixdex_tui::run_tui;

use crate::default_db_dir;
use clap::Parser;
use std::path::PathBuf;

/// Options for `nixdex tui`.
#[derive(Debug, Parser)]
pub struct TuiOpts {
    /// Directory where the index is stored.
    #[arg(short, long = "db", default_value = default_db_dir(), env = "NIX_INDEX_DATABASE")]
    pub database: PathBuf,
}
