//! Building a nixdex index from nixpkgs and the binary cache.

use std::path::PathBuf;
use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::CACHE_URL;
use crate::database::Writer;
use crate::errors::{Error, Result};
use crate::hydra::Fetcher;
use crate::listings;
use crate::nixpkgs;
use crate::path_cache::PathCache;

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
    /// Ignore the existing `paths.cache` and re-fetch all store paths.
    pub force: bool,
    /// Cache-key used to identify a `paths.cache` file; defaults to `nixpkgs`.
    pub cache_key: Option<String>,
    /// Synthesize `/bin/<mainProgram>` listings from `meta.mainProgram`.
    pub main_program: bool,
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
            force: false,
            cache_key: None,
            main_program: true,
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

        std::fs::create_dir_all(&opts.database).map_err(|source| Error::CreateDatabaseDir {
            path: opts.database.clone(),
            source,
        })?;

        let cache_path = opts.database.join("paths.cache");
        let cache_key = if let Some(key) = opts.cache_key.as_deref() {
            key
        } else {
            &opts.nixpkgs
        };
        let path_cache: Option<Arc<Mutex<PathCache>>> = if opts.path_cache {
            if opts.force {
                Some(Arc::new(Mutex::new(PathCache::new(cache_key))))
            } else {
                match PathCache::load(&cache_path, cache_key) {
                    Ok(Some(cache)) => Some(Arc::new(Mutex::new(cache))),
                    Ok(None) => {
                        info!(cache_key, "path cache not found or stale; starting fresh");
                        Some(Arc::new(Mutex::new(PathCache::new(cache_key))))
                    }
                    Err(err) => {
                        warn!(error = %err, "failed to load path cache; starting fresh");
                        Some(Arc::new(Mutex::new(PathCache::new(cache_key))))
                    }
                }
            }
        } else {
            None
        };

        let db_file = opts.database.join("files");
        let mut writer =
            Writer::create_with_version(&db_file, opts.compression_level, opts.format_version)
                .map_err(|source| Error::CreateDatabase {
                    path: db_file.clone(),
                    source: Box::new(source),
                })?;

        let progress = MultiProgress::new();

        // Root set + each extra scope (mirrors upstream nix-index multi-query).
        let eval_pb = progress.add(ProgressBar::new_spinner());
        eval_pb.set_message("Evaluating nixpkgs...");
        let packages = nixpkgs::list_packages_with_scopes(
            &opts.nixpkgs,
            opts.system.as_deref(),
            &opts.extra_scopes,
            opts.show_trace,
            opts.main_program,
        )
        .await
        .map_err(|source| Error::QueryPackages {
            source: Box::new(source),
        })?;
        eval_pb.finish_with_message(format!("Evaluated {} package(s)", packages.packages.len()));

        let fetcher = Fetcher::new(CACHE_URL).map_err(|err| {
            Error::Io(std::io::Error::other(format!(
                "failed to create binary-cache client: {err}"
            )))
        })?;

        let jobs = opts.jobs.max(1);
        let filter_prefix = opts.filter_prefix.as_bytes().to_vec();

        let starting_set = packages
            .packages
            .into_iter()
            .flat_map(|pkg| {
                let main_program = pkg.main_program.clone();
                pkg.store_paths.into_iter().map(move |path| {
                    let mp = if path.origin().output == "out" {
                        main_program.clone()
                    } else {
                        None
                    };
                    listings::PackageEntry {
                        path,
                        main_program: mp,
                    }
                })
            })
            .collect();

        let fetch_pb = progress.add(ProgressBar::new_spinner());
        fetch_pb.set_message("Fetching listings...");

        let mut listings =
            listings::fetch_listings(&fetcher, jobs, starting_set, path_cache.clone())
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
                Ok((store_path, tree)) => {
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
            fetch_pb.set_message(format!("Indexed {indexed}, failed {failed}"));
            fetch_pb.inc(1);
        }
        fetch_pb.finish_with_message(format!("Fetched {indexed} listings ({failed} failed)"));

        let cached = if let Some(pc) = path_cache.as_ref() {
            pc.lock().await.hits
        } else {
            0
        };

        if opts.path_cache {
            match path_cache {
                Some(pc) => {
                    let cache = pc.lock().await;
                    if let Err(err) = cache.save(&cache_path) {
                        warn!(error = %err, "failed to save path cache");
                    }
                }
                None => warn!("path cache enabled but not initialized"),
            }
        }

        let size = writer.finish().map_err(|source| Error::WriteDatabase {
            path: db_file.clone(),
            source: Box::new(source),
        })?;

        info!(
            indexed,
            failed,
            cached,
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
        assert!(opts.main_program);
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
            force: false,
            cache_key: None,
            main_program: true,
            extra_scopes: vec![],
        };
        IndexBuilder::new(opts).build().await.expect("build");
        assert!(dir.path().join("files").exists());
    }
}
