//! Tool for generating a nixdex database.

use clap::Parser;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    nixdex_cli::index::run(nixdex_cli::index::Args::parse()).await
}
