//! Creating and searching NIXI-compatible file databases.
//!
//! Format (compatible with upstream `nix-index`):
//! - magic `NIXI` + `u64` LE version `1`
//! - zstd stream of frcode blocks
//! - per package: file entries, then footer entry with metadata `p` and JSON `StorePath`

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use regex::bytes::Regex;
use thiserror::Error;

use indexmap::IndexSet;

use crate::files::{FileNode, FileTree, FileTreeEntry, FileType};
use crate::frcode;
use crate::store_path::StorePath;

/// Database format version supported by this build.
const FORMAT_VERSION: u64 = 1;

/// Magic bytes identifying a nix-index / nixdex database file.
const FILE_MAGIC: &[u8] = b"NIXI";

/// Errors that can occur when reading or writing a database.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Encountered an unsupported on-disk file type marker (bad magic).
    #[error(
        "expected file to start with nix-index file magic 'NIXI', but found '{found:?}' (is this a valid nix-index database file?)"
    )]
    UnsupportedFileType {
        /// Raw type marker found in the database.
        found: Vec<u8>,
    },

    /// Database format version is newer (or older) than supported.
    #[error(
        "this executable only supports the nix-index database version {}, but found a database with version {found}",
        FORMAT_VERSION
    )]
    UnsupportedVersion {
        /// Version number found in the header.
        found: u64,
    },

    /// Package entry required by a file listing was missing.
    #[error("database corrupt: missing package entry")]
    MissingPackageEntry,

    /// frcode codec reported a corrupt stream.
    #[error("database corrupt, frcode error: {0}")]
    Frcode(#[from] frcode::Error),

    /// A file entry could not be parsed.
    #[error("database corrupt: could not parse entry: {entry:?}")]
    EntryParse {
        /// Raw entry bytes.
        entry: Vec<u8>,
    },

    /// A store-path JSON blob could not be parsed.
    #[error("database corrupt: could not parse store path: {path:?}")]
    StorePathParse {
        /// Raw store-path blob.
        path: Vec<u8>,
    },

    /// Regular expression compilation failed.
    #[error("invalid search pattern: {0}")]
    Regex(#[from] regex::Error),

    /// redb reported a storage failure (legacy / reserved).
    #[error("redb error: {0}")]
    Redb(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failed.
    #[error("JSON error: {0}")]
    Json(String),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias for this module.
pub type Result<T> = std::result::Result<T, Error>;

/// Writer that creates a new NIXI database file.
pub struct Writer {
    /// Inner encoder; `None` after finish / drop.
    writer: Option<BufWriter<zstd::Encoder<'static, File>>>,
}

impl Drop for Writer {
    fn drop(&mut self) {
        if self.writer.is_some() {
            // Best-effort finish; callers should prefer `finish()` for error reporting.
            let _ = self.finish_encoder();
        }
    }
}

impl Writer {
    /// Creates a new database at the given path with the specified zstd compression level.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be created or the header written.
    pub fn create<P: AsRef<Path>>(path: P, level: i32) -> Result<Self> {
        let mut file = File::create(path)?;
        file.write_all(FILE_MAGIC)?;
        file.write_u64::<LittleEndian>(FORMAT_VERSION)?;
        let encoder = zstd::Encoder::new(file, level)?;
        // Multithreading is optional; ignore failure on platforms that disallow it.
        // (zstd::Encoder::multithread returns Result on some versions.)
        Ok(Self {
            writer: Some(BufWriter::new(encoder)),
        })
    }

    /// Add a package and its file tree to the database.
    ///
    /// Entries are only added if their path starts with `filter_prefix`.
    /// Packages with no matching entries are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding or writing fails.
    pub fn add(
        &mut self,
        path: &StorePath,
        files: &FileTree,
        filter_prefix: &[u8],
    ) -> Result<()> {
        let entries = files.to_list(filter_prefix);
        if entries.is_empty() {
            return Ok(());
        }

        let writer = self.writer.as_mut().ok_or_else(|| {
            Error::Io(io::Error::other("database writer already finished"))
        })?;

        let json = sonic_rs::to_vec(path).map_err(|err| Error::Json(err.to_string()))?;
        let mut encoder = frcode::Encoder::new(writer, b"p".to_vec(), json)?;
        for entry in entries {
            entry.encode(&mut encoder)?;
        }
        encoder.finish()?;
        Ok(())
    }

    fn finish_encoder(&mut self) -> Result<File> {
        let writer = self.writer.take().ok_or_else(|| {
            Error::Io(io::Error::other("database writer already finished"))
        })?;
        let encoder = writer
            .into_inner()
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(encoder.finish()?)
    }

    /// Finish writing and return the compressed size in bytes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the stream cannot be finalized.
    pub fn finish(mut self) -> Result<u64> {
        let mut file = self.finish_encoder()?;
        Ok(file.stream_position()?)
    }
}

/// Reader that opens an existing NIXI database file.
pub struct Reader {
    decoder: frcode::Decoder<BufReader<zstd::Decoder<'static, BufReader<File>>>>,
    path: PathBuf,
}

impl Reader {
    /// Opens a nix-index / nixdex database located at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the path does not exist or is not a valid database.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let mut file = File::open(&path_buf)?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;

        if magic != FILE_MAGIC {
            return Err(Error::UnsupportedFileType {
                found: magic.to_vec(),
            });
        }

        let version = file.read_u64::<LittleEndian>()?;
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion { found: version });
        }

        let decoder = zstd::Decoder::new(file)?;
        Ok(Self {
            decoder: frcode::Decoder::new(BufReader::new(decoder)),
            path: path_buf,
        })
    }

    /// Return the path this reader was opened against.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Linearly scan the database, yielding `(StorePath, FileTreeEntry)` matches.
    ///
    /// Wave 3 uses a simple linear frcode scan. An FST-backed path index may be
    /// layered on later for sub-millisecond cold queries.
    ///
    /// # Errors
    ///
    /// Returns an error if the database stream is corrupt or I/O fails.
    pub fn search_entries(
        &mut self,
        path_pattern: &Regex,
        package_pattern: Option<&Regex>,
        hash: Option<&str>,
    ) -> Result<Vec<(StorePath, FileTreeEntry)>> {
        let mut matches = Vec::new();
        // Current package being accumulated: entries first, package marker last.
        let mut pending: Vec<FileTreeEntry> = Vec::new();

        loop {
            let block = self.decoder.decode()?;
            if block.is_empty() {
                break;
            }

            for line in block.split(|c| *c == b'\n') {
                if line.is_empty() {
                    continue;
                }

                // Package terminator: metadata starts with 'p' and path is JSON.
                if line.starts_with(b"p\0") {
                    let Some(json) = line.get(2..) else {
                        return Err(Error::StorePathParse {
                            path: line.to_vec(),
                        });
                    };
                    let pkg: StorePath =
                        sonic_rs::from_slice(json).map_err(|_| Error::StorePathParse {
                            path: json.to_vec(),
                        })?;

                    let accept_pkg = package_pattern
                        .is_none_or(|re| re.is_match(pkg.name().as_bytes()))
                        && hash.is_none_or(|h| h == pkg.hash());

                    if accept_pkg {
                        for entry in std::mem::take(&mut pending) {
                            if path_pattern.is_match(&entry.path) {
                                matches.push((pkg.clone(), entry));
                            }
                        }
                    } else {
                        pending.clear();
                    }
                    continue;
                }

                let entry = FileTreeEntry::decode(line).map_err(|_| Error::EntryParse {
                    entry: line.to_vec(),
                })?;
                pending.push(entry);
            }
        }

        if !pending.is_empty() {
            return Err(Error::MissingPackageEntry);
        }

        Ok(matches)
    }

    /// Scaffold for a future FST-backed query path.
    ///
    /// # Errors
    ///
    /// Currently always returns [`Error::NotImplemented`].
    pub fn query_fst(&self, _pattern: &str) -> Result<Vec<String>> {
        // FST secondary index is planned for a later wave; Wave 3 uses linear scan.
        Err(Error::NotImplemented(
            "database::Reader::query_fst is reserved for a future FST index",
        ))
    }
}

/// Output mode for a search request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Full output with package/path details.
    Full {
        /// Emit ANSI colors in output.
        color: bool,
        /// Group matches that share the same matching path component.
        group: bool,
        /// Only print matches from top-level packages.
        only_toplevel: bool,
    },
    /// Print only attribute names.
    Minimal,
}

/// Options for a database search.
#[derive(Debug, Clone)]
pub struct SearchOptions<'a> {
    /// Directory that holds the index database.
    pub database: PathBuf,
    /// Pattern to search for (regex-ready string from the CLI).
    pub pattern: String,
    /// Restrict results to a store-path hash.
    pub hash: Option<String>,
    /// Restrict results to package names matching this pattern.
    pub package_pattern: Option<String>,
    /// File-type filter (empty means "all types").
    pub file_type: &'a [FileType],
    /// Output formatting mode.
    pub mode: SearchMode,
}

/// Search the database for entries matching the supplied options and print them.
///
/// # Errors
///
/// Returns an error if the database cannot be read or the pattern is invalid.
#[allow(clippy::print_stdout)] // search is a CLI-facing printer for now
pub fn search(options: &SearchOptions<'_>) -> crate::Result<()> {
    let index_file = options.database.join("files");
    let mut reader = Reader::open(&index_file).map_err(|source| crate::Error::ReadDatabase {
        path: index_file.clone(),
        source: Box::new(source),
    })?;

    let path_pattern = Regex::new(&options.pattern).map_err(|err| {
        crate::Error::Parse(format!("invalid path pattern '{}': {err}", options.pattern))
    })?;
    let package_re = match &options.package_pattern {
        Some(pat) => Some(Regex::new(pat).map_err(|err| {
            crate::Error::Parse(format!("invalid package pattern '{pat}': {err}"))
        })?),
        None => None,
    };

    let results = reader
        .search_entries(
            &path_pattern,
            package_re.as_ref(),
            options.hash.as_deref(),
        )
        .map_err(|source| crate::Error::ReadDatabase {
            path: index_file,
            source: Box::new(source),
        })?;

    // Track printed attrs for --minimal de-duplication (ordered set).
    let mut printed_attrs: IndexSet<String> = IndexSet::new();

    for (store_path, FileTreeEntry { path, node }) in results {
        // Grouping: only print if the last regex match ends in the final path component.
        let group = matches!(options.mode, SearchMode::Full { group: true, .. });
        if group
            && path_pattern
                .find_iter(&path)
                .last()
                .is_some_and(|m| path.get(m.end()..).is_some_and(|rest| rest.contains(&b'/')))
        {
            continue;
        }

        let only_toplevel = matches!(
            options.mode,
            SearchMode::Full {
                only_toplevel: true,
                ..
            }
        );
        if only_toplevel && !store_path.origin().toplevel {
            continue;
        }

        let entry_type = node.get_type();
        if !options.file_type.is_empty() && !options.file_type.contains(&entry_type) {
            continue;
        }

        let mut attr = format!(
            "{}.{}",
            store_path.origin().attr,
            store_path.origin().output
        );
        if !store_path.origin().toplevel {
            attr = format!("({attr})");
        }

        match options.mode {
            SearchMode::Minimal => {
                if printed_attrs.insert(attr.clone()) {
                    println!("{attr}");
                }
            }
            SearchMode::Full { color, .. } => {
                let (typ, size) = match &node {
                    FileNode::Regular { executable, size } => {
                        (if *executable { "x" } else { "r" }, *size)
                    }
                    FileNode::Directory { size, .. } => ("d", *size),
                    FileNode::Symlink { .. } => ("s", 0),
                };
                let size_str = format_grouped(size);
                print!("{attr:<40} {size_str:>14} {typ:>1} {}", store_path.as_str());

                let path_str = String::from_utf8_lossy(&path);
                if color {
                    // Highlight all non-empty matches in the path.
                    let mut prev = 0usize;
                    let bytes = path_str.as_bytes();
                    for mat in path_pattern.find_iter(bytes) {
                        if mat.start() == mat.end() {
                            continue;
                        }
                        // Safe because we only slice on byte offsets from the same str.
                        if let (Some(before), Some(matched)) = (
                            path_str.get(prev..mat.start()),
                            path_str.get(mat.start()..mat.end()),
                        ) {
                            print!("{before}\x1b[31m{matched}\x1b[0m");
                        }
                        prev = mat.end();
                    }
                    if let Some(rest) = path_str.get(prev..) {
                        println!("{rest}");
                    } else {
                        println!();
                    }
                } else {
                    println!("{path_str}");
                }
            }
        }
    }

    Ok(())
}

/// Format an integer with thousands separators (e.g. `16_524` → `"16,524"`).
fn format_grouped(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        // Insert a comma before every group of three digits counting from the right.
        let remaining = bytes.len() - i;
        if i > 0 && remaining.is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(*b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileTree;
    use crate::store_path::Origin;
    use bytes::Bytes;

    fn sample_store_path() -> StorePath {
        StorePath::new(
            "/nix/store".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "hello-2.12".into(),
            Origin {
                attr: "hello".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        )
    }

    fn sample_tree() -> FileTree {
        FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![(
                Bytes::from_static(b"hello"),
                FileTree::regular(64472, true),
            )]),
        )])
    }

    #[test]
    fn writer_reader_roundtrip_and_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&path, &tree, b"").expect("add");
            let size = writer.finish().expect("finish");
            assert!(size > 0);
        }

        // Magic check
        let mut f = File::open(&db_path).expect("open");
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic).expect("magic");
        assert_eq!(&magic, b"NIXI");

        let mut reader = Reader::open(&db_path).expect("reader");
        let re = Regex::new("bin/hello").expect("regex");
        let hits = reader
            .search_entries(&re, None, None)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.name(), "hello-2.12");
        assert_eq!(hits[0].1.path, b"/bin/hello");

        // Public search() printer
        let options = SearchOptions {
            database: dir.path().to_path_buf(),
            pattern: "bin/hello".into(),
            hash: None,
            package_pattern: None,
            file_type: &[],
            mode: SearchMode::Minimal,
        };
        search(&options).expect("search ok");
    }

    #[test]
    fn filter_prefix_skips_non_matching() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");
        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 1).expect("create");
            // No entries under /lib → package should be omitted entirely.
            writer.add(&path, &tree, b"/lib").expect("add");
            writer.finish().expect("finish");
        }

        let mut reader = Reader::open(&db_path).expect("reader");
        let re = Regex::new(".*").expect("regex");
        let hits = reader.search_entries(&re, None, None).expect("search");
        assert!(hits.is_empty());
    }

    #[test]
    fn format_grouped_numbers() {
        assert_eq!(format_grouped(0), "0");
        assert_eq!(format_grouped(12), "12");
        assert_eq!(format_grouped(1234), "1,234");
        assert_eq!(format_grouped(16_524), "16,524");
        assert_eq!(format_grouped(1_000_000), "1,000,000");
    }
}
