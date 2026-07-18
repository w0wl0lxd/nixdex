//! File-tree data types used while indexing store paths.

use std::io::Write;
use std::str;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::frcode;

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
        matches!(
            self,
            Self::Regular {
                executable: true,
                ..
            }
        )
    }

    /// Return the size of this node (0 for symlinks).
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Self::Regular { size, .. } | Self::Directory { size, .. } => *size,
            Self::Symlink { .. } => 0,
        }
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

impl FileNode<()> {
    /// Write NIXI metadata for this node (size/target + type tag) into `encoder`.
    ///
    /// # Errors
    ///
    /// Returns an error when the encoder rejects the metadata bytes.
    pub fn encode_meta<W: Write>(
        &self,
        encoder: &mut frcode::Encoder<W>,
    ) -> Result<(), frcode::Error> {
        match self {
            Self::Regular { executable, size } => {
                let tag = if *executable { "x" } else { "r" };
                let meta = format!("{size}{tag}");
                encoder.write_meta(meta.as_bytes())?;
            }
            Self::Symlink { target } => {
                encoder.write_meta(target)?;
                encoder.write_meta(b"s")?;
            }
            Self::Directory { size, contents: () } => {
                let meta = format!("{size}d");
                encoder.write_meta(meta.as_bytes())?;
            }
        }
        Ok(())
    }

    /// Decode a content-free node from NIXI metadata bytes (without the path).
    #[must_use]
    pub fn decode_meta(buf: &[u8]) -> Option<Self> {
        let (kind, rest) = buf.split_last()?;
        match *kind {
            b'x' | b'r' => {
                let executable = *kind == b'x';
                let size = str::from_utf8(rest).ok()?.parse().ok()?;
                Some(Self::Regular { executable, size })
            }
            b's' => Some(Self::Symlink {
                target: Bytes::copy_from_slice(rest),
            }),
            b'd' => {
                let size = str::from_utf8(rest).ok()?.parse().ok()?;
                Some(Self::Directory { size, contents: () })
            }
            _ => None,
        }
    }
}

impl FileTreeEntry {
    /// Returns `true` if the entry can be safely encoded into an frcode
    /// stream. Paths and symlink targets must not contain NUL or newline,
    /// both of which are reserved by the frcode format.
    #[must_use]
    pub fn is_encodable(&self) -> bool {
        let forbidden = |b: &u8| *b == b'\0' || *b == b'\n';
        if self.path.iter().any(forbidden) {
            return false;
        }
        if let FileNode::Symlink { target } = &self.node
            && target.iter().any(forbidden)
        {
            return false;
        }
        true
    }

    /// Encode the entry into an frcode stream (`metadata\\0path\\n`).
    ///
    /// # Errors
    ///
    /// Returns an error when writing to the encoder fails.
    pub fn encode<W: Write>(self, encoder: &mut frcode::Encoder<W>) -> Result<(), frcode::Error> {
        self.node.encode_meta(encoder)?;
        encoder.write_path(self.path)?;
        Ok(())
    }

    /// Decode an entry from a decoded frcode line (`metadata\\0path`).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Parse`] when the buffer is not a valid entry.
    pub fn decode(buf: &[u8]) -> crate::Result<Self> {
        let sep = memchr::memchr(b'\0', buf)
            .ok_or_else(|| crate::Error::Parse("file entry missing NUL separator".to_string()))?;
        let node_bytes = buf.get(..sep).ok_or_else(|| {
            crate::Error::Parse("file entry metadata slice out of range".to_string())
        })?;
        let path = buf
            .get(sep + 1..)
            .ok_or_else(|| crate::Error::Parse("file entry path slice out of range".to_string()))?;
        let node = FileNode::decode_meta(node_bytes).ok_or_else(|| {
            crate::Error::Parse(format!("invalid file entry metadata: {node_bytes:?}"))
        })?;
        Ok(Self {
            path: path.to_vec(),
            node,
        })
    }
}

/// Maximum decompressed size accepted for a `.ls` JSON document (defensive cap).
pub(crate) const MAX_LS_BYTES: usize = 32 * 1024 * 1024; // 32 MiB

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

    /// Parse a binary-cache `.ls` JSON document into a file tree.
    ///
    /// Accepts either the full document (`{"root":{...}}`) or a bare node object
    /// (`{"type":"directory","entries":{...}}`).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Parse`] when the JSON is invalid, oversized, too deep,
    /// or the schema is unexpected.
    pub fn from_ls_json(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() > MAX_LS_BYTES {
            return Err(crate::Error::Parse(format!(
                ".ls payload too large: {} bytes (max {MAX_LS_BYTES})",
                bytes.len()
            )));
        }
        from_ls_json_inner(bytes, 0).map_err(crate::Error::Parse)
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

/// JSON shape of a cache.nixos.org `.ls` node.
#[derive(Debug, Deserialize)]
struct LsNode {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    executable: Option<bool>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    entries: Option<indexmap::IndexMap<String, Self>>,
}

#[derive(Debug, Deserialize)]
struct LsRoot {
    root: LsNode,
}

fn from_ls_json_inner(bytes: &[u8], depth: u32) -> Result<FileTree, String> {
    // Prefer full document with `root`; fall back to bare node.
    if let Ok(doc) = sonic_rs::from_slice::<LsRoot>(bytes) {
        return ls_node_to_tree(doc.root, depth);
    }
    let node: LsNode =
        sonic_rs::from_slice(bytes).map_err(|err| format!(".ls JSON parse: {err}"))?;
    ls_node_to_tree(node, depth)
}

const MAX_LS_DEPTH: u32 = 64;

fn ls_node_to_tree(node: LsNode, depth: u32) -> Result<FileTree, String> {
    if depth > MAX_LS_DEPTH {
        return Err(format!(".ls tree exceeds max depth {MAX_LS_DEPTH}"));
    }
    match node.kind.as_str() {
        "regular" => {
            let size = match node.size {
                Some(n) => n,
                None => 0,
            };
            let executable = matches!(node.executable, Some(true));
            Ok(FileTree::regular(size, executable))
        }
        "symlink" => {
            let target = node
                .target
                .ok_or_else(|| "symlink node missing target".to_string())?;
            Ok(FileTree::symlink(Bytes::from(target.into_bytes())))
        }
        "directory" => {
            let map = match node.entries {
                Some(m) => m,
                None => indexmap::IndexMap::new(),
            };
            let mut entries = Vec::with_capacity(map.len());
            for (name, child) in map {
                // Directory entry names from the cache must be single path components.
                if name.contains('/') || name.contains('\\') || name == ".." || name == "." {
                    return Err(format!("invalid directory entry name: {name:?}"));
                }
                let child_tree = ls_node_to_tree(child, depth + 1)?;
                entries.push((Bytes::from(name.into_bytes()), child_tree));
            }
            Ok(FileTree::directory(entries))
        }
        other => Err(format!("unknown .ls node type: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_ls_bin() {
        let json = br#"{"entries":{"hello":{"executable":true,"size":64472,"type":"regular"}},"type":"directory"}"#;
        let tree = from_ls_json_inner(json, 0).expect("parse");
        let list = tree.to_list(b"");
        assert!(list.iter().any(|e| e.path == b"/hello"));
        let hello = list.iter().find(|e| e.path == b"/hello").expect("hello");
        assert!(hello.node.is_executable());
    }

    #[test]
    fn parse_full_root_document() {
        let json = br#"{"root":{"type":"directory","entries":{"bin":{"type":"directory","entries":{"hello":{"type":"regular","size":1,"executable":true}}}}}}"#;
        let tree = from_ls_json_inner(json, 0).expect("parse");
        let list = tree.to_list(b"/bin");
        assert!(list.iter().any(|e| e.path == b"/bin/hello"));
    }

    #[test]
    fn rejects_traversal_entry_names() {
        let json = br#"{"type":"directory","entries":{"..":{"type":"regular","size":1,"executable":false}}}"#;
        assert!(from_ls_json_inner(json, 0).is_err());
    }

    #[test]
    fn file_tree_entry_encode_decode_roundtrip() {
        let cases = [
            FileTreeEntry {
                path: b"/bin/hello".to_vec(),
                node: FileNode::Regular {
                    size: 64472,
                    executable: true,
                },
            },
            FileTreeEntry {
                path: b"/share/doc".to_vec(),
                node: FileNode::Directory {
                    size: 3,
                    contents: (),
                },
            },
            FileTreeEntry {
                path: b"/bin/sh".to_vec(),
                node: FileNode::Symlink {
                    target: Bytes::from_static(b"bash"),
                },
            },
            FileTreeEntry {
                path: b"/etc/foo".to_vec(),
                node: FileNode::Regular {
                    size: 12,
                    executable: false,
                },
            },
        ];

        for entry in cases {
            let mut buf = Vec::new();
            {
                let mut enc =
                    frcode::Encoder::new(&mut buf, b"p".to_vec(), b"{}".to_vec()).expect("enc");
                entry.clone().encode(&mut enc).expect("encode");
                enc.finish().expect("finish");
            }
            // Decode the first line without the package footer / trailing newline.
            let line = buf.split(|b| *b == b'\n').next().expect("line").to_vec();
            // Expand frcode: single-entry encoder emits metadata\0diff path
            // Re-decode via Decoder for a faithful roundtrip.
            let mut dec = frcode::Decoder::new(std::io::Cursor::new(&buf));
            let block = dec.decode().expect("decode block");
            let entry_line = block.split(|b| *b == b'\n').next().expect("entry line");
            // Strip trailing empty etc — the entry line is metadata\0path
            // but still has no trailing newline in the split piece.
            let decoded = FileTreeEntry::decode(entry_line).expect("decode entry");
            assert_eq!(decoded, entry);
            let _ = line;
        }
    }

    #[test]
    fn from_ls_json_malformed_json() {
        let invalid_json = b"{not valid json";
        let result = FileTree::from_ls_json(invalid_json);
        assert!(result.is_err());
    }

    #[test]
    fn from_ls_json_oversized_payload() {
        let mut large_json = vec![b' '; MAX_LS_BYTES + 1];
        *large_json.first_mut().expect("first byte") = b'{';
        *large_json.last_mut().expect("last byte") = b'}';
        let result = FileTree::from_ls_json(&large_json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn from_ls_json_deep_nesting_exceeds_max() {
        // Create a deeply nested structure that exceeds MAX_LS_DEPTH
        let mut json = String::from(r#"{"type":"directory","entries":{""#);
        for _ in 0..MAX_LS_DEPTH + 1 {
            json.push_str(r#""a":{"type":"directory","entries":{""#);
        }
        json.push_str(r#""b":{"type":"regular","size":1}}}"#);
        for _ in 0..MAX_LS_DEPTH + 1 {
            json.push_str("}}");
        }

        let result = FileTree::from_ls_json(json.as_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn from_ls_json_invalid_entry_name_with_slash() {
        let json = br#"{"type":"directory","entries":{"a/b":{"type":"regular","size":1}}}"#;
        let result = FileTree::from_ls_json(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid directory entry name")
        );
    }

    #[test]
    fn from_ls_json_invalid_entry_name_with_backslash() {
        let json = br#"{"type":"directory","entries":{"a\\b":{"type":"regular","size":1}}}"#;
        let result = FileTree::from_ls_json(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid directory entry name")
        );
    }

    #[test]
    fn from_ls_json_invalid_entry_name_dot() {
        let json = br#"{"type":"directory","entries":{".":{"type":"regular","size":1}}}"#;
        let result = FileTree::from_ls_json(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid directory entry name")
        );
    }

    #[test]
    fn from_ls_json_unknown_node_type() {
        let json = br#"{"type":"unknown","size":1}"#;
        let result = FileTree::from_ls_json(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown .ls node type")
        );
    }

    #[test]
    fn from_ls_json_symlink_missing_target() {
        let json = br#"{"type":"symlink"}"#;
        let result = FileTree::from_ls_json(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing target"));
    }

    #[test]
    fn to_list_prefix_filtering() {
        let tree = FileTree::directory(vec![
            (
                Bytes::from_static(b"bin"),
                FileTree::directory(vec![
                    (Bytes::from_static(b"ls"), FileTree::regular(100, true)),
                    (Bytes::from_static(b"cat"), FileTree::regular(50, false)),
                ]),
            ),
            (
                Bytes::from_static(b"share"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"doc"),
                    FileTree::directory(vec![(
                        Bytes::from_static(b"README"),
                        FileTree::regular(10, false),
                    )]),
                )]),
            ),
        ]);

        // Filter for /bin paths
        let bin_entries = tree.to_list(b"/bin");
        assert!(bin_entries.iter().all(|e| e.path.starts_with(b"/bin")));
        assert!(bin_entries.iter().any(|e| e.path == b"/bin/ls"));
        assert!(bin_entries.iter().any(|e| e.path == b"/bin/cat"));
        assert!(!bin_entries.iter().any(|e| e.path.starts_with(b"/share")));

        // Filter for /share paths
        let share_entries = tree.to_list(b"/share");
        assert!(share_entries.iter().all(|e| e.path.starts_with(b"/share")));
        assert!(share_entries.iter().any(|e| e.path == b"/share/doc/README"));

        // Empty filter returns all
        let all_entries = tree.to_list(b"");
        assert_eq!(all_entries.len(), 7); // /, /bin, /share, /bin/ls, /bin/cat, /share/doc, /share/doc/README
    }

    #[test]
    fn to_list_empty_prefix_matches_all() {
        let tree = FileTree::directory(vec![
            (Bytes::from_static(b"a"), FileTree::regular(1, false)),
            (Bytes::from_static(b"b"), FileTree::regular(2, false)),
        ]);

        let entries = tree.to_list(b"");
        assert_eq!(entries.len(), 3); // /, /a, /b
    }

    #[test]
    fn to_list_nonexistent_prefix_returns_empty() {
        let tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::regular(1, false),
        )]);

        let entries = tree.to_list(b"/nonexistent");
        assert!(entries.is_empty());
    }

    #[test]
    fn file_node_decode_meta_invalid_format() {
        // Missing type suffix
        assert!(FileNode::decode_meta(b"123").is_none());
        // Invalid type character
        assert!(FileNode::decode_meta(b"123z").is_none());
        // Non-numeric size
        assert!(FileNode::decode_meta(b"abcx").is_none());
    }

    #[test]
    fn file_tree_entry_decode_missing_nul() {
        let invalid = b"1r/bin/ls"; // No NUL separator
        let result = FileTreeEntry::decode(invalid);
        assert!(result.is_err());
    }

    #[test]
    fn file_tree_entry_decode_invalid_metadata() {
        let invalid = b"invalid\x00/bin/ls";
        let result = FileTreeEntry::decode(invalid);
        assert!(result.is_err());
    }

    #[test]
    fn is_encodable_rejects_nul_or_newline() {
        let good = FileTreeEntry {
            path: b"/bin/ls".to_vec(),
            node: FileNode::Regular {
                size: 1,
                executable: true,
            },
        };
        assert!(good.is_encodable());

        let bad_path = FileTreeEntry {
            path: b"/bin/b\nad".to_vec(),
            node: FileNode::Regular {
                size: 1,
                executable: true,
            },
        };
        assert!(!bad_path.is_encodable());

        let bad_target = FileTreeEntry {
            path: b"/bin/link".to_vec(),
            node: FileNode::Symlink {
                target: Bytes::from_static(b"/etc/passwd\n"),
            },
        };
        assert!(!bad_target.is_encodable());
    }
}
