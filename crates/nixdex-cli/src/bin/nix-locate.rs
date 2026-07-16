//! Tool for searching for files in nixpkgs packages.

use clap::Parser;

fn main() -> color_eyre::Result<()> {
    nixdex_cli::locate::run(nixdex_cli::locate::Opts::parse())
}
