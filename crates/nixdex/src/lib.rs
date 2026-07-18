//! `nixdex` — umbrella crate for the nixdex project.
//!
//! nixdex is a modern rewrite of `nix-index` / `nix-locate`, providing
//! fast Nix package file indexing and search.
//!
//! The functionality is split across workspace crates:
//! - `nixdex-core`: core indexing and search library
//! - `nixdex-cli`: `nix-index`, `nix-locate`, and `nixdex` command-line tools
//! - `nixdex-daemon`: optional background HTTP daemon

#[doc(inline)]
pub use nixdex_core::*;
