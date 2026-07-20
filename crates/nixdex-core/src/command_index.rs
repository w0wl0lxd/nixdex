//! Command-provider secondary index: FST map + postings of package providers.
//!
//! Unlike the general `files` database and the `redb` exact-path index, this
//! index answers only one question — *which packages provide the executable
//! `foo` found in a bin directory?* — and it answers it without ever touching
//! the `files` blob or deserializing a path cache. That keeps
//! command-not-found lookups well under a millisecond even on the full
//! nixpkgs database, where the `redb` reader alone costs hundreds of
//! milliseconds on every cold query.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.commands.fst` — [`fst::Map`] from command bytes → cookie (`u64`)
//! - `files.commands.postings` — cookie points at a packed provider-id list
//! - `files.commands.providers` — packed `(attr, output, toplevel)` table

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, WriteBytesExt};
use fst::Map;
use mmap_guard;
use thiserror::Error;

/// Directories whose immediate children are treated as executable commands.
const BIN_PREFIXES: &[&[u8]] = &[b"/bin/", b"/usr/bin/", b"/sbin/", b"/usr/sbin/"];

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NCPO";
/// Magic for the provider table.
const PROVIDERS_MAGIC: &[u8] = b"NCMD";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum total size of the provider table sidecar (defensive cap).
const MAX_PROVIDERS_BYTES: usize = 256 * 1024 * 1024;

/// Maximum number of providers in the table sidecar.
const MAX_PROVIDER_COUNT: usize = 2_000_000;

/// Maximum length of a single attr or output label.
const MAX_LABEL_BYTES: usize = 64 * 1024;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum number of providers returned for a single command.
const MAX_PROVIDERS_PER_COMMAND: usize = 1_000_000;

/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 128 * 1024 * 1024;

/// Maximum number of distinct commands indexed.
const MAX_COMMAND_COUNT: usize = 4_000_000;

/// Sidecar basenames relative to the database directory.
pub const FST_FILE: &str = "files.commands.fst";
/// Postings table filename.
pub const POSTINGS_FILE: &str = "files.commands.postings";
/// Provider table filename.
pub const PROVIDERS_FILE: &str = "files.commands.providers";

/// Errors while building or querying the command secondary index.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Secondary index files are missing from the database directory.
    #[error("command secondary index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("command secondary index corrupt: {0}")]
    Corrupt(String),

    /// FST crate reported a build or query error.
    #[error("fst error: {0}")]
    Fst(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// A package that provides an executable command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProvider {
    /// Attribute path, e.g. `gitMinimal`.
    pub attr: String,
    /// Output name, e.g. `out` or `man`.
    pub output: String,
    /// Whether the package is reachable from the top-level package set.
    ///
    /// CNF consumers can sort top-level packages first, mirroring Filkoll.
    pub toplevel: bool,
}

/// Accumulates command → provider mappings while a NIXI database is written.
#[derive(Debug, Default)]
pub struct CommandIndexBuilder {
    /// Deduplicated provider records.
    providers: Vec<(String, String, bool)>,
    /// `(attr, output, toplevel)` → provider id, for deduplication.
    provider_index: BTreeMap<(String, String, bool), u32>,
    /// command (final path component under a bin dir) → provider ids.
    commands: BTreeMap<Vec<u8>, Vec<u32>>,
}

impl CommandIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a package and every command it provides from its absolute paths.
    ///
    /// Returns the assigned provider id. Paths that are not executable
    /// candidates under a bin directory are ignored.
    pub fn record_package(
        &mut self,
        package_attr: String,
        package_output: String,
        toplevel: bool,
        paths: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<u32> {
        let provider_id = self.provider_id(package_attr, package_output, toplevel)?;
        for path in paths {
            if !is_command_candidate(&path) {
                continue;
            }
            let cmd = basename_of(&path).to_vec();
            if cmd.is_empty() {
                continue;
            }
            self.commands.entry(cmd).or_default().push(provider_id);
        }
        Ok(provider_id)
    }

    fn provider_id(&mut self, attr: String, output: String, toplevel: bool) -> Result<u32> {
        let key = (attr.clone(), output.clone(), toplevel);
        if let Some(id) = self.provider_index.get(&key) {
            return Ok(*id);
        }
        let id = u32::try_from(self.providers.len())
            .map_err(|_| Error::Corrupt("provider count overflow".into()))?;
        self.providers.push((attr, output, toplevel));
        self.provider_index.insert(key, id);
        Ok(id)
    }

    /// Number of distinct commands recorded.
    #[must_use]
    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if any sidecar cannot be written or the FST build fails.
    pub fn write_sidecars(&self, db_dir: &Path) -> Result<()> {
        if self.commands.len() > MAX_COMMAND_COUNT {
            return Err(Error::Corrupt(format!(
                "too many commands: {} (max {MAX_COMMAND_COUNT})",
                self.commands.len()
            )));
        }

        write_provider_table(&db_dir.join(PROVIDERS_FILE), &self.providers)?;

        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        // BTreeMap keeps commands sorted — required by `fst::MapBuilder`.
        let mut cookies: Vec<(Vec<u8>, u64)> = Vec::with_capacity(self.commands.len());
        for (cmd, ids) in &self.commands {
            let mut ids: Vec<u32> = ids.clone();
            ids.sort_unstable();
            ids.dedup();
            let count = u32::try_from(ids.len())
                .map_err(|_| Error::Corrupt("too many providers for one command".into()))?;
            let cookie = u64::try_from(raw.len())
                .map_err(|_| Error::Corrupt("postings cookie overflow".into()))?;
            raw.extend_from_slice(&count.to_le_bytes());
            for id in ids {
                raw.extend_from_slice(&id.to_le_bytes());
            }
            cookies.push((cmd.clone(), cookie));
        }
        std::fs::write(db_dir.join(POSTINGS_FILE), &raw)?;

        let mut builder = fst::MapBuilder::memory();
        for (cmd, cookie) in &cookies {
            builder
                .insert(cmd, *cookie)
                .map_err(|err| Error::Fst(err.to_string()))?;
        }
        let fst_bytes = builder
            .into_inner()
            .map_err(|err| Error::Fst(err.to_string()))?;
        std::fs::write(db_dir.join(FST_FILE), fst_bytes)?;
        Ok(())
    }
}

/// Opened command secondary index for exact-command queries.
#[derive(Debug)]
pub struct CommandIndex {
    map: Map<mmap_guard::FileData>,
    postings: mmap_guard::FileData,
    providers: mmap_guard::FileData,
    // Per-provider byte ranges into `providers`, plus the toplevel flag.
    provider_ranges: Vec<(usize, usize, usize, usize, bool)>,
}

impl CommandIndex {
    /// Open sidecars from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Missing`] when any required sidecar is absent, or
    /// [`Error::Corrupt`] / [`Error::Fst`] when the files cannot be parsed.
    pub fn open(db_dir: &Path) -> Result<Self> {
        let fst_path = db_dir.join(FST_FILE);
        let postings_path = db_dir.join(POSTINGS_FILE);
        let providers_path = db_dir.join(PROVIDERS_FILE);

        if !fst_path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {FST_FILE}"),
            });
        }
        if !postings_path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {POSTINGS_FILE}"),
            });
        }
        if !providers_path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {PROVIDERS_FILE}"),
            });
        }

        let fst_data = mmap_guard::map_file(&fst_path).map_err(Error::Io)?;
        if fst_data.len() > MAX_FST_BYTES {
            return Err(Error::Corrupt("fst file too large".into()));
        }
        let map = Map::new(fst_data).map_err(|err| Error::Fst(err.to_string()))?;

        let postings = mmap_guard::map_file(&postings_path).map_err(Error::Io)?;
        if postings.len() > MAX_POSTINGS_BYTES {
            return Err(Error::Corrupt("postings file too large".into()));
        }
        validate_postings_header(&postings)?;

        let providers = mmap_guard::map_file(&providers_path).map_err(Error::Io)?;
        if providers.len() > MAX_PROVIDERS_BYTES {
            return Err(Error::Corrupt("provider table too large".into()));
        }
        let provider_ranges = parse_provider_ranges(&providers)?;

        Ok(Self {
            map,
            postings,
            providers,
            provider_ranges,
        })
    }

    /// Look up the packages that provide an exact command name.
    ///
    /// Returns an empty list when the command is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn lookup_command(&self, command: &[u8]) -> Result<Vec<CommandProvider>> {
        let Some(cookie) = self.map.get(command) else {
            return Ok(Vec::new());
        };
        let ids = read_provider_ids_at(&self.postings, cookie)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let index = usize::try_from(id)
                .map_err(|_| Error::Corrupt(format!("provider id {id} does not fit usize")))?;
            let Some((a_start, a_end, o_start, o_end, toplevel)) = self.provider_ranges.get(index)
            else {
                return Err(Error::Corrupt(format!(
                    "provider id {id} out of range (providers={})",
                    self.provider_ranges.len()
                )));
            };
            let attr_bytes = self
                .providers
                .get(*a_start..*a_end)
                .ok_or_else(|| Error::Corrupt(format!("provider {id} attr range out of bounds")))?;
            let output_bytes = self.providers.get(*o_start..*o_end).ok_or_else(|| {
                Error::Corrupt(format!("provider {id} output range out of bounds"))
            })?;
            let attr = std::str::from_utf8(attr_bytes)
                .map_err(|e| Error::Corrupt(format!("provider {id} invalid UTF-8 attr: {e}")))?
                .to_string();
            let output = std::str::from_utf8(output_bytes)
                .map_err(|e| Error::Corrupt(format!("provider {id} invalid UTF-8 output: {e}")))?
                .to_string();
            out.push(CommandProvider {
                attr,
                output,
                toplevel: *toplevel,
            });
        }
        Ok(out)
    }

    /// Number of providers in the table.
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.provider_ranges.len()
    }
}

/// Whether `path` is an immediate child of a bin directory (an executable
/// command candidate). Leading-slash absolute store paths only.
#[must_use]
pub fn is_command_candidate(path: &[u8]) -> bool {
    for prefix in BIN_PREFIXES {
        if path.len() <= prefix.len() || !path.starts_with(prefix) {
            continue;
        }
        let Some(rest) = path.get(prefix.len()..) else {
            continue;
        };
        // The command is the single trailing filename; reject nested paths.
        if !rest.is_empty() && !rest.contains(&b'/') {
            return true;
        }
    }
    false
}

/// Final component of a store-relative path (`/bin/ls` → `ls`).
#[must_use]
pub fn basename_of(path: &[u8]) -> &[u8] {
    match memchr::memrchr(b'/', path) {
        Some(i) => match path.get(i + 1..) {
            Some(rest) => rest,
            None => &[],
        },
        None => path,
    }
}

fn read_u32_le(bytes: &[u8], at: usize) -> Result<u32> {
    let end = at
        .checked_add(4)
        .ok_or_else(|| Error::Corrupt("u32 offset overflow".into()))?;
    let slice = bytes
        .get(at..end)
        .ok_or_else(|| Error::Corrupt(format!("u32 read past end at {at}")))?;
    let arr: [u8; 4] = slice
        .try_into()
        .map_err(|_| Error::Corrupt("u32 slice length".into()))?;
    Ok(u32::from_le_bytes(arr))
}

fn validate_postings_header(postings: &[u8]) -> Result<()> {
    if postings.len() < POSTINGS_MAGIC.len() + 4 {
        return Err(Error::Corrupt("postings too short".into()));
    }
    let magic = postings
        .get(..POSTINGS_MAGIC.len())
        .ok_or_else(|| Error::Corrupt("postings too short for magic".into()))?;
    if magic != POSTINGS_MAGIC {
        return Err(Error::Corrupt(format!(
            "postings magic {magic:?}, expected {:?}",
            POSTINGS_MAGIC
        )));
    }
    let ver = read_u32_le(postings, POSTINGS_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(Error::Corrupt(format!(
            "postings version {ver}, expected {SIDE_VERSION}"
        )));
    }
    Ok(())
}

fn read_provider_ids_at(postings: &[u8], cookie: u64) -> Result<Vec<u32>> {
    let start = usize::try_from(cookie)
        .map_err(|_| Error::Corrupt(format!("cookie {cookie} does not fit usize")))?;
    let count = usize::try_from(read_u32_le(postings, start)?)
        .map_err(|_| Error::Corrupt("provider count too large".into()))?;
    if count > MAX_PROVIDERS_PER_COMMAND {
        return Err(Error::Corrupt(format!(
            "too many providers for one command: {count} (max {MAX_PROVIDERS_PER_COMMAND})"
        )));
    }
    let body = start
        .checked_add(4)
        .ok_or_else(|| Error::Corrupt("provider list offset overflow".into()))?;
    let need = count
        .checked_mul(4)
        .and_then(|b| body.checked_add(b))
        .ok_or_else(|| Error::Corrupt("provider list size overflow".into()))?;
    if need > postings.len() {
        return Err(Error::Corrupt(format!(
            "provider list truncated: need {need}, have {}",
            postings.len()
        )));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = body + i * 4;
        out.push(read_u32_le(postings, off)?);
    }
    Ok(out)
}

fn write_provider_table(path: &Path, providers: &[(String, String, bool)]) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(PROVIDERS_MAGIC)?;
    w.write_u32::<LittleEndian>(SIDE_VERSION)?;
    w.write_u32::<LittleEndian>(
        u32::try_from(providers.len()).map_err(|_| Error::Corrupt("too many providers".into()))?,
    )?;
    for (attr, output, toplevel) in providers {
        let attr_bytes = attr.as_bytes();
        let output_bytes = output.as_bytes();
        if attr_bytes.len() > MAX_LABEL_BYTES {
            return Err(Error::Corrupt(format!(
                "attr label too long: {}",
                attr_bytes.len()
            )));
        }
        if output_bytes.len() > MAX_LABEL_BYTES {
            return Err(Error::Corrupt(format!(
                "output label too long: {}",
                output_bytes.len()
            )));
        }
        w.write_u32::<LittleEndian>(
            u32::try_from(attr_bytes.len()).map_err(|_| Error::Corrupt("attr too long".into()))?,
        )?;
        w.write_all(attr_bytes)?;
        w.write_u32::<LittleEndian>(
            u32::try_from(output_bytes.len())
                .map_err(|_| Error::Corrupt("output too long".into()))?,
        )?;
        w.write_all(output_bytes)?;
        w.write_u8(u8::from(*toplevel))?;
    }
    w.flush()?;
    Ok(())
}

/// Parse the provider sidecar into byte ranges without cloning strings.
fn parse_provider_ranges(bytes: &[u8]) -> Result<Vec<(usize, usize, usize, usize, bool)>> {
    let magic = bytes
        .get(..PROVIDERS_MAGIC.len())
        .ok_or(Error::Corrupt("providers too short for magic".into()))?;
    if magic != PROVIDERS_MAGIC {
        return Err(Error::Corrupt(format!(
            "providers magic {magic:?}, expected {:?}",
            PROVIDERS_MAGIC
        )));
    }

    let ver = read_u32_le(bytes, PROVIDERS_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(Error::Corrupt(format!(
            "providers version {ver}, expected {SIDE_VERSION}"
        )));
    }

    let count = usize::try_from(read_u32_le(bytes, PROVIDERS_MAGIC.len() + 4)?)
        .map_err(|_| Error::Corrupt("provider count too large".into()))?;
    if count > MAX_PROVIDER_COUNT {
        return Err(Error::Corrupt(format!(
            "provider count too large: {count} (max {MAX_PROVIDER_COUNT})"
        )));
    }
    let header_size = PROVIDERS_MAGIC.len() + 4 + 4;
    if count
        .checked_mul(4)
        .is_none_or(|need| need > bytes.len().saturating_sub(header_size))
    {
        return Err(Error::Corrupt("provider count too large".into()));
    }

    let mut ranges = Vec::with_capacity(count);
    let mut pos = header_size;
    for _ in 0..count {
        let attr_len = usize::try_from(read_u32_le(bytes, pos)?)
            .map_err(|_| Error::Corrupt("attr length too large".into()))?;
        if attr_len > MAX_LABEL_BYTES {
            return Err(Error::Corrupt(format!("attr label too long: {attr_len}")));
        }
        let attr_start = pos + 4;
        let attr_end = attr_start
            .checked_add(attr_len)
            .ok_or(Error::Corrupt("attr length overflow".into()))?;
        if attr_end > bytes.len() {
            return Err(Error::Corrupt("attr truncated".into()));
        }

        let out_start = attr_end;
        let out_len = usize::try_from(read_u32_le(bytes, out_start)?)
            .map_err(|_| Error::Corrupt("output length too large".into()))?;
        if out_len > MAX_LABEL_BYTES {
            return Err(Error::Corrupt(format!("output label too long: {out_len}")));
        }
        let out_body = out_start + 4;
        let out_end = out_body
            .checked_add(out_len)
            .ok_or(Error::Corrupt("output length overflow".into()))?;
        if out_end + 1 > bytes.len() {
            return Err(Error::Corrupt("output truncated".into()));
        }

        let toplevel = match bytes.get(out_end) {
            Some(0) => false,
            Some(1) => true,
            Some(v) => {
                return Err(Error::Corrupt(format!("invalid toplevel flag: {v}")));
            }
            None => return Err(Error::Corrupt("toplevel flag truncated".into())),
        };

        ranges.push((attr_start, attr_end, out_body, out_end, toplevel));
        pos = out_end + 1;
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_command_candidate_paths() {
        assert!(is_command_candidate(b"/bin/ls"));
        assert!(is_command_candidate(b"/usr/bin/python3"));
        assert!(is_command_candidate(b"/sbin/init"));
        assert!(is_command_candidate(b"/usr/sbin/sshd"));
        assert!(!is_command_candidate(b"/bin")); // no trailing filename
        assert!(!is_command_candidate(b"/bin/")); // empty filename
        assert!(!is_command_candidate(b"/lib/ls")); // not a bin dir
        assert!(!is_command_candidate(b"/bin/sub/ls")); // nested
        assert!(!is_command_candidate(b"ls")); // not absolute
    }

    #[test]
    fn build_and_query_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = CommandIndexBuilder::new();
        builder
            .record_package(
                "git.out".into(),
                "out".into(),
                true,
                vec![b"/usr/bin/git".to_vec(), b"/bin/git".to_vec()],
            )
            .expect("pkg0");
        builder
            .record_package(
                "busybox.out".into(),
                "out".into(),
                false,
                vec![b"/bin/git".to_vec()],
            )
            .expect("pkg1");
        builder
            .record_package(
                "python3.out".into(),
                "out".into(),
                true,
                vec![b"/usr/bin/python3".to_vec()],
            )
            .expect("pkg2");
        builder.write_sidecars(dir.path()).expect("write");

        let index = CommandIndex::open(dir.path()).expect("open");
        assert_eq!(index.provider_count(), 3);

        let mut git = index.lookup_command(b"git").expect("git");
        git.sort_by(|a, b| a.attr.cmp(&b.attr));
        assert_eq!(
            git,
            vec![
                CommandProvider {
                    attr: "busybox.out".into(),
                    output: "out".into(),
                    toplevel: false,
                },
                CommandProvider {
                    attr: "git.out".into(),
                    output: "out".into(),
                    toplevel: true,
                },
            ]
        );

        let py = index.lookup_command(b"python3").expect("python3");
        assert_eq!(
            py,
            vec![CommandProvider {
                attr: "python3.out".into(),
                output: "out".into(),
                toplevel: true,
            }]
        );

        let missing = index.lookup_command(b"nope").expect("nope");
        assert!(missing.is_empty());
    }

    #[test]
    fn dedup_providers_and_commands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = CommandIndexBuilder::new();
        // Same provider recorded twice via two paths must collapse to one id.
        builder
            .record_package(
                "dup.out".into(),
                "out".into(),
                true,
                vec![b"/bin/a".to_vec()],
            )
            .expect("p0");
        builder
            .record_package(
                "dup.out".into(),
                "out".into(),
                true,
                vec![b"/bin/b".to_vec()],
            )
            .expect("p1");
        builder.write_sidecars(dir.path()).expect("write");

        let index = CommandIndex::open(dir.path()).expect("open");
        assert_eq!(index.provider_count(), 1);

        let a = index.lookup_command(b"a").expect("a");
        assert_eq!(a.len(), 1);
        let b = index.lookup_command(b"b").expect("b");
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn open_missing_sidecar_is_missing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = CommandIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
    }
}
