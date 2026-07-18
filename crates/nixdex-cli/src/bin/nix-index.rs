//! Tool for generating a nixdex database.
//!
//! This binary is a drop-in replacement for `nix-index`, so it defaults to the
//! upstream `~/.cache/nix-index` database directory unless the user explicitly
//! passes `-d`/`--db`.

use clap::{CommandFactory, FromArgMatches};
use std::ffi::OsStr;
use std::sync::OnceLock;

fn nix_index_default_db_dir() -> &'static OsStr {
    static CACHE: OnceLock<std::ffi::OsString> = OnceLock::new();
    CACHE
        .get_or_init(|| nixdex_core::nix_index_dir().into_os_string())
        .as_os_str()
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let mut cmd = nixdex_cli::index::Args::command();
    cmd = cmd.mut_arg("database", |arg| {
        arg.default_value_os(nix_index_default_db_dir())
    });
    let matches = cmd.get_matches();
    let args = nixdex_cli::index::Args::from_arg_matches(&matches)?;
    nixdex_cli::index::run(args).await
}
