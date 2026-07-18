//! Tool for generating a nixdex database.
//!
//! This binary is a drop-in replacement for `nix-index`, so it defaults to the
//! upstream `~/.cache/nix-index` database directory unless the user explicitly
//! passes `-d`/`--db`.

use clap::{CommandFactory, FromArgMatches};
use std::sync::OnceLock;

fn nix_index_default_db_dir() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            nixdex_core::nix_index_dir()
                .into_os_string()
                .into_string()
                .unwrap_or_else(|_| String::from("/tmp/nix-index"))
        })
        .as_str()
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let mut cmd = nixdex_cli::index::Args::command();
    cmd = cmd.mut_arg("database", |arg| {
        arg.default_value(nix_index_default_db_dir())
    });
    let matches = cmd.get_matches();
    let args = nixdex_cli::index::Args::from_arg_matches(&matches)?;
    nixdex_cli::index::run(args).await
}
