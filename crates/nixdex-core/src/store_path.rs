//! Nix store path and origin metadata.

use serde::{Deserialize, Serialize};

/// Describes how a store path was discovered during evaluation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    /// Attribute path (for example `hello.out`).
    pub attr: String,
    /// Derivation output name (for example `out`).
    pub output: String,
    /// Whether this path was produced by a top-level attribute.
    pub toplevel: bool,
    /// Optional system triple used during evaluation.
    pub system: Option<String>,
}

/// Alias kept for call sites that used the older name.
pub type PathOrigin = Origin;

/// A single Nix store path — the output of a derivation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    /// Store directory, typically `/nix/store`.
    store_dir: String,
    /// Content-addressed hash prefix of the path.
    hash: String,
    /// Package name including version, without the hash.
    name: String,
    /// How this path was discovered.
    origin: Origin,
}

impl StorePath {
    /// Construct a store path from already-parsed components.
    #[must_use]
    pub fn new(store_dir: String, hash: String, name: String, origin: Origin) -> Self {
        Self {
            store_dir,
            hash,
            name,
            origin,
        }
    }

    /// Parse an absolute store path using the supplied origin metadata.
    ///
    /// Returns `None` when the input does not look like a store path of the form
    /// `{store_dir}/{hash}-{name}`.
    #[must_use]
    pub fn parse(origin: Origin, path: &str) -> Option<Self> {
        let (prefix, name) = path.split_once('-')?;
        let (store_dir, hash) = match prefix.rsplit_once('/') {
            Some((dir, hash)) => (dir, hash),
            None => ("", prefix),
        };

        Some(Self {
            store_dir: store_dir.to_string(),
            hash: hash.to_string(),
            name: name.to_string(),
            origin,
        })
    }

    /// Package name without the hash prefix.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Content-addressed hash of the store path.
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Store directory that contains this path.
    #[must_use]
    pub fn store_dir(&self) -> &str {
        &self.store_dir
    }

    /// Origin metadata describing how this path was found.
    #[must_use]
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Render the path as `{store_dir}/{hash}-{name}`.
    #[must_use]
    pub fn as_str(&self) -> String {
        format!("{}/{}-{}", self.store_dir, self.hash, self.name)
    }
}

impl std::fmt::Display for StorePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}-{}", self.store_dir, self.hash, self.name)
    }
}
