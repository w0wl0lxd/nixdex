//! Building a nixdex index from nixpkgs and the binary cache.

use std::path::PathBuf;

use crate::errors::{Error, Result};

/// Options controlling an index build.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Number of concurrent HTTP requests.
    pub jobs: usize,
    /// Directory where the index database is stored.
    pub database: PathBuf,
    /// nixpkgs path/expression (for example `<nixpkgs>`).
    pub nixpkgs: String,
    /// Optional system triple for evaluation.
    pub system: Option<String>,
    /// Zstandard compression level for the on-disk database.
    pub compression_level: i32,
    /// Pass `--show-trace` to Nix evaluation.
    pub show_trace: bool,
    /// Only index paths starting with this prefix.
    pub filter_prefix: String,
    /// Persist intermediate fetch results into `paths.cache`.
    pub path_cache: bool,
    /// Extra attribute scopes to walk during evaluation.
    pub extra_scopes: Vec<String>,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            jobs: 100,
            database: PathBuf::from("/tmp/nix-index"),
            nixpkgs: String::from("<nixpkgs>"),
            system: None,
            compression_level: 22,
            show_trace: false,
            filter_prefix: String::new(),
            path_cache: false,
            extra_scopes: vec![
                String::from("haskellPackages"),
                String::from("rPackages"),
                String::from("coqPackages"),
                String::from("texlive.pkgs"),
            ],
        }
    }
}

/// Stub builder that will orchestrate eval → fetch → write.
#[derive(Debug, Default)]
pub struct IndexBuilder {
    options: UpdateOptions,
}

impl IndexBuilder {
    /// Create a builder with the supplied options.
    #[must_use]
    pub fn new(options: UpdateOptions) -> Self {
        Self { options }
    }

    /// Borrow the configured options.
    #[must_use]
    pub fn options(&self) -> &UpdateOptions {
        &self.options
    }

    /// Run the index build.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until the pipeline is complete.
    #[allow(clippy::unused_async)] // will await eval/fetch once the pipeline lands
    pub async fn build(&self) -> Result<()> {
        let _ = &self.options;
        Err(Error::NotImplemented(
            "IndexBuilder::build is not implemented yet",
        ))
    }
}

/// Build or update the nixdex index.
///
/// # Errors
///
/// Returns an error if the index build fails or is not yet implemented.
pub async fn update(options: &UpdateOptions) -> Result<()> {
    let builder = IndexBuilder::new(options.clone());
    builder.build().await
}
