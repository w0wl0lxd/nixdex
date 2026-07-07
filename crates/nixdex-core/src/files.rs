//! File-tree data types used while indexing store paths.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// The kind of a file node inside a store path.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FileType {
    /// Regular file; `executable` indicates the executable bit.
    Regular {
        /// Whether the file has the executable bit set.
        executable: bool,
    },
    /// Directory entry.
    Directory,
    /// Symbolic link.
    Symlink,
}

#[cfg(feature = "cli")]
impl clap::ValueEnum for FileType {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::Regular { executable: false },
            Self::Regular { executable: true },
            Self::Directory,
            Self::Symlink,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        match self {
            Self::Regular { executable: false } => Some(clap::builder::PossibleValue::new("r")),
            Self::Regular { executable: true } => Some(clap::builder::PossibleValue::new("x")),
            Self::Directory => Some(clap::builder::PossibleValue::new("d")),
            Self::Symlink => Some(clap::builder::PossibleValue::new("s")),
        }
    }
}

impl std::str::FromStr for FileType {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "r" => Ok(Self::Regular { executable: false }),
            "x" => Ok(Self::Regular { executable: true }),
            "d" => Ok(Self::Directory),
            "s" => Ok(Self::Symlink),
            _ => Err("invalid file type (expected r, x, d, or s)"),
        }
    }
}

/// All representable file types, used when no type filter is given.
pub const ALL_FILE_TYPES: &[FileType] = &[
    FileType::Regular { executable: true },
    FileType::Regular { executable: false },
    FileType::Directory,
    FileType::Symlink,
];

/// A single node in a file tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileNode<T> {
    /// Regular file with size and executable bit.
    Regular {
        /// File size in bytes.
        size: u64,
        /// Whether the file is executable.
        executable: bool,
    },
    /// Symbolic link pointing at `target`.
    Symlink {
        /// Symlink target as raw bytes.
        target: Bytes,
    },
    /// Directory with an entry count of `size` and payload `contents`.
    Directory {
        /// Number of direct children.
        size: u64,
        /// Directory payload (entry list, or `()` after splitting).
        contents: T,
    },
}

impl<T> FileNode<T> {
    /// Split the node into a content-free node and an optional contents reference.
    #[must_use]
    pub fn split_contents(&self) -> (FileNode<()>, Option<&T>) {
        match self {
            Self::Regular { size, executable } => (
                FileNode::Regular {
                    size: *size,
                    executable: *executable,
                },
                None,
            ),
            Self::Symlink { target } => (
                FileNode::Symlink {
                    target: target.clone(),
                },
                None,
            ),
            Self::Directory { size, contents } => (
                FileNode::Directory {
                    size: *size,
                    contents: (),
                },
                Some(contents),
            ),
        }
    }

    /// Return the [`FileType`] of this node.
    #[must_use]
    pub fn get_type(&self) -> FileType {
        match self {
            Self::Regular { executable, .. } => FileType::Regular {
                executable: *executable,
            },
            Self::Directory { .. } => FileType::Directory,
            Self::Symlink { .. } => FileType::Symlink,
        }
    }

    /// Returns whether this node is an executable regular file.
    #[must_use]
    pub fn is_executable(&self) -> bool {
        matches!(self, Self::Regular { executable: true, .. })
    }
}

/// Directory contents as a sorted vector of `(name, subtree)` pairs.
pub type FileEntries = Vec<(Bytes, FileTree)>;

/// A full tree of files belonging to a single store path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTree(FileNode<FileEntries>);

/// A flattened entry produced when listing a file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTreeEntry {
    /// Absolute path inside the store path (starts with `/`).
    pub path: Vec<u8>,
    /// Content-free node for this entry.
    pub node: FileNode<()>,
}

impl FileTreeEntry {
    /// Encode the entry as bytes for storage.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::NotImplemented`] until the on-disk codec lands.
    pub fn encode<W: std::io::Write>(&self, _writer: &mut W) -> crate::Result<()> {
        Err(crate::Error::NotImplemented(
            "FileTreeEntry::encode is not implemented yet",
        ))
    }

    /// Decode an entry from a byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::NotImplemented`] until the on-disk codec lands.
    pub fn decode(_buf: &[u8]) -> crate::Result<Self> {
        Err(crate::Error::NotImplemented(
            "FileTreeEntry::decode is not implemented yet",
        ))
    }
}

impl FileTree {
    /// Create a regular file tree node.
    #[must_use]
    pub fn regular(size: u64, executable: bool) -> Self {
        Self(FileNode::Regular { size, executable })
    }

    /// Create a symlink file tree node.
    #[must_use]
    pub fn symlink(target: Bytes) -> Self {
        Self(FileNode::Symlink { target })
    }

    /// Create a directory file tree node. Entries are sorted by name.
    #[must_use]
    pub fn directory(mut entries: FileEntries) -> Self {
        entries.sort_by(|a, b| Ord::cmp(&a.0, &b.0));
        let size = match u64::try_from(entries.len()) {
            Ok(n) => n,
            Err(_) => u64::MAX,
        };
        Self(FileNode::Directory {
            size,
            contents: entries,
        })
    }

    /// List all entries whose path starts with `filter_prefix`.
    #[must_use]
    pub fn to_list(&self, filter_prefix: &[u8]) -> Vec<FileTreeEntry> {
        let mut result = Vec::new();
        let mut stack = Vec::with_capacity(16);
        stack.push((Vec::<u8>::new(), self));

        while let Some((path, tree)) = stack.pop() {
            let (node, contents) = tree.0.split_contents();
            if let Some(entries) = contents {
                for (name, child) in entries {
                    let mut child_path = path.clone();
                    child_path.push(b'/');
                    child_path.extend_from_slice(name);
                    stack.push((child_path, child));
                }
            }
            if path.starts_with(filter_prefix) {
                result.push(FileTreeEntry { path, node });
            }
        }

        result
    }

    /// Borrow the root node.
    #[must_use]
    pub fn root(&self) -> &FileNode<FileEntries> {
        &self.0
    }
}
