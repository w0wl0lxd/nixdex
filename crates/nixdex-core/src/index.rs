//! Building a nixdex index from nixpkgs and the binary cache.

use std::path::PathBuf;

use tracing::{info, warn};

use crate::CACHE_URL;
use crate::database::Writer;
use crate::errors::{Error, Result};
use crate::hydra::Fetcher;
use crate::listings;
use crate::nixpkgs;

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
    /// On-disk database format version (1 or 2).
    pub format_version: u64,
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
            format_version: 2,
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

/// Orchestrates eval → fetch → write for a nixdex index.
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

    /// Run the index build: evaluate packages, fetch `.ls` trees, write NIXI DB.
    ///
    /// # Errors
    ///
    /// Returns an error if the database directory cannot be created, evaluation
    /// fails hard, or the writer cannot be finalized.
    pub async fn build(&self) -> Result<()> {
        let opts = &self.options;

        if opts.path_cache {
            return Err(Error::NotImplemented(
                "--path-cache is not implemented yet (refusing to silently no-op)",
            ));
        }

        std::fs::create_dir_all(&opts.database).map_err(|source| Error::CreateDatabaseDir {
            path: opts.database.clone(),
            source,
        })?;

        let db_file = opts.database.join("files");
        let mut writer =
            Writer::create_with_version(&db_file, opts.compression_level, opts.format_version)
                .map_err(|source| Error::CreateDatabase {
                    path: db_file.clone(),
                    source: Box::new(source),
                })?;

        // Root set + each extra scope (mirrors upstream nix-index multi-query).
        let packages = nixpkgs::list_packages_with_scopes(
            &opts.nixpkgs,
            opts.system.as_deref(),
            &opts.extra_scopes,
            opts.show_trace,
        )
        .await
        .map_err(|source| Error::QueryPackages {
            source: Box::new(source),
        })?;

        info!(
            packages = packages.store_paths.len(),
            scopes = opts.extra_scopes.len(),
            "listed store paths from nixpkgs"
        );

        let fetcher = Fetcher::new(CACHE_URL).map_err(|err| {
            Error::Io(std::io::Error::other(format!(
                "failed to create binary-cache client: {err}"
            )))
        })?;

        let jobs = opts.jobs.max(1);
        let filter_prefix = opts.filter_prefix.as_bytes().to_vec();

        let mut listings = listings::fetch_listings(&fetcher, jobs, packages.store_paths)
            .await
            .map_err(|source| {
                Error::Io(std::io::Error::other(format!(
                    "failed to start listing fetcher: {source}"
                )))
            })?;

        let mut indexed = 0usize;
        let mut failed = 0usize;
        while let Some(result) = listings.recv().await {
            match result {
                Ok((store_path, _nar_path, tree)) => {
                    writer
                        .add(&store_path, &tree, &filter_prefix)
                        .map_err(|source| Error::WriteDatabase {
                            path: db_file.clone(),
                            source: Box::new(source),
                        })?;
                    indexed += 1;
                }
                Err(err) => {
                    warn!(error = %err, "closure fetch yielded an error; skipping");
                    failed += 1;
                }
            }
        }

        let size = writer.finish().map_err(|source| Error::WriteDatabase {
            path: db_file.clone(),
            source: Box::new(source),
        })?;

        info!(
            indexed,
            failed,
            size_bytes = size,
            db = %db_file.display(),
            "index build complete"
        );
        Ok(())
    }
}

/// Build or update the nixdex index.
///
/// # Errors
///
/// Returns an error if the index build fails.
pub async fn update(options: &UpdateOptions) -> Result<()> {
    let builder = IndexBuilder::new(options.clone());
    builder.build().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_match_upstream_baseline() {
        let opts = UpdateOptions::default();
        assert_eq!(opts.jobs, 100);
        assert_eq!(opts.compression_level, 22);
        assert_eq!(opts.format_version, 2);
        assert_eq!(opts.nixpkgs, "<nixpkgs>");
        assert!(!opts.path_cache);
    }

    /// Networked end-to-end smoke test against the public binary cache.
    /// Marked `ignore` so the default suite stays offline.
    #[tokio::test]
    #[ignore = "requires network + nix-eval-jobs"]
    async fn index_hello_from_nixpkgs() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Evaluate a tiny attrset rather than all of nixpkgs.
        let expr = r"{ hello = (import <nixpkgs> {}).hello; }";
        let opts = UpdateOptions {
            jobs: 4,
            database: dir.path().to_path_buf(),
            nixpkgs: expr.to_string(),
            system: None,
            compression_level: 3,
            format_version: 1,
            show_trace: false,
            filter_prefix: "/bin/".into(),
            path_cache: false,
            extra_scopes: vec![],
        };
        IndexBuilder::new(opts).build().await.expect("build");
        assert!(dir.path().join("files").exists());
    }
}
