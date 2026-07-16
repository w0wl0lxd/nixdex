//! Compact prefix-differential encoding for file-tree entries (frcode).
//!
//! Compatible with the extended frcode format used by upstream `nix-index`:
//! each line is `metadata\\0<shared-prefix-diff><path-suffix>\\n`.

use std::cmp;
use std::io::{self, BufRead, Write};
use std::ops::{Deref, DerefMut};

use thiserror::Error;

/// Errors that can occur while encoding or decoding frcode data.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Underlying I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Shared prefix length is outside the valid range for the previous path.
    #[error(
        "length of shared prefix must be >= 0 and <= {previous_len} (length of previous item), but found: {shared_len}"
    )]
    SharedOutOfRange {
        /// Length of the previous path.
        previous_len: usize,
        /// Requested shared length.
        shared_len: isize,
    },

    /// Shared prefix length overflowed when applying a differential.
    #[error("length of shared prefix too big: cannot add {shared_len} to {diff} without overflow")]
    SharedOverflow {
        /// Current shared length.
        shared_len: isize,
        /// Differential that would overflow.
        diff: isize,
    },

    /// Entry metadata was not terminated by a NUL byte.
    #[error("missing terminating NUL byte for entry")]
    MissingNul,

    /// Entry was not terminated by a newline.
    #[error("missing newline separator for entry")]
    MissingNewline,

    /// Shared-prefix differential bytes were missing.
    #[error("missing the shared prefix length differential for entry")]
    MissingPrefixDifferential,

    /// Metadata or path contained a forbidden byte.
    #[error("entry contains forbidden byte (NUL or newline)")]
    ForbiddenByte,
}

type Result<T> = std::result::Result<T, Error>;

/// Buffer that can optionally grow while decoding incomplete entries.
struct ResizableBuf {
    allow_resize: bool,
    data: Vec<u8>,
}

impl ResizableBuf {
    fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity],
            allow_resize: true,
        }
    }

    fn resize(&mut self, new_size: usize) -> bool {
        if new_size <= self.data.len() {
            return true;
        }
        if !self.allow_resize {
            return false;
        }
        self.data.resize(new_size, b'\x00');
        true
    }
}

impl Deref for ResizableBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl DerefMut for ResizableBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

/// Decoder for the frcode format. Yields blocks of decoded entries.
pub struct Decoder<R> {
    reader: R,
    last_path: usize,
    partial_entry_start: usize,
    shared_len: isize,
    buf: ResizableBuf,
    pos: usize,
}

impl<R: BufRead> Decoder<R> {
    /// Construct a new decoder over `reader`.
    #[must_use]
    pub fn new(reader: R) -> Self {
        let capacity = 1_000_000;
        Self {
            reader,
            buf: ResizableBuf::new(capacity),
            pos: 0,
            last_path: 0,
            shared_len: 0,
            partial_entry_start: 0,
        }
    }

    fn copy_shared(&mut self) -> Result<bool> {
        let shared_len = self.shared_len as usize;
        let new_pos = self.pos + shared_len;
        let new_last_path = self.pos;
        if !self.buf.resize(new_pos) {
            return Ok(false);
        }

        if self.shared_len < 0 || self.last_path + shared_len > self.pos {
            return Err(Error::SharedOutOfRange {
                previous_len: self.pos - self.last_path,
                shared_len: self.shared_len,
            });
        }

        let (_, last) = self.buf.split_at_mut(self.last_path);
        let (last, new) = last.split_at_mut(self.pos - self.last_path);
        if let Some(dst) = new.get_mut(..shared_len) {
            if let Some(src) = last.get(..shared_len) {
                dst.copy_from_slice(src);
            } else {
                return Err(Error::SharedOutOfRange {
                    previous_len: self.pos - self.last_path,
                    shared_len: self.shared_len,
                });
            }
        } else {
            return Err(Error::SharedOutOfRange {
                previous_len: self.pos - self.last_path,
                shared_len: self.shared_len,
            });
        }

        self.pos += shared_len;
        self.last_path = new_last_path;
        Ok(true)
    }

    /// Read until NUL. Returns `Ok(Some(true))` when a NUL was found,
    /// `Ok(Some(false))` when the output buffer is full, and `Ok(None)` on EOF
    /// with no further input.
    fn read_to_nul(&mut self) -> Result<Option<bool>> {
        loop {
            let (done, len) = {
                let &mut Self {
                    ref mut reader,
                    ref mut buf,
                    ref mut pos,
                    ..
                } = self;
                let input = match reader.fill_buf() {
                    Ok(data) => data,
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::from(e)),
                };

                if input.is_empty() {
                    return Ok(None);
                }

                let (done, len) = match memchr::memchr(b'\x00', input) {
                    Some(i) => (true, i + 1),
                    None => (false, input.len()),
                };

                let new_pos = *pos + len;
                if buf.resize(new_pos) {
                    if let (Some(dst), Some(src)) = (buf.get_mut(*pos..new_pos), input.get(..len)) {
                        dst.copy_from_slice(src);
                    }
                    *pos = new_pos;
                    (done, len)
                } else {
                    return Ok(Some(false));
                }
            };
            self.reader.consume(len);
            if done {
                return Ok(Some(true));
            }
        }
    }

    fn decode_prefix_diff(&mut self) -> Result<i16> {
        let mut buf = [0; 1];
        self.reader
            .read_exact(&mut buf)
            .map_err(|_| Error::MissingPrefixDifferential)?;

        match buf[0] {
            0x80 => {
                let mut ext = [0; 2];
                self.reader
                    .read_exact(&mut ext)
                    .map_err(|_| Error::MissingPrefixDifferential)?;
                let high = i16::from(ext[0]);
                let low = i16::from(ext[1]);
                Ok(high << 8 | low)
            }
            b => Ok(i16::from(b.cast_signed())),
        }
    }

    /// Decode as many complete entries as fit into the internal buffer.
    ///
    /// Returns an empty slice when the input has been fully consumed.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is corrupt or I/O fails.
    pub fn decode(&mut self) -> Result<&mut [u8]> {
        let end = self.pos;
        self.pos = 0;

        let mut copy_pos = cmp::min(self.partial_entry_start, self.last_path);
        let item_start = self.partial_entry_start - copy_pos;
        self.last_path -= copy_pos;

        // Source and destination may overlap; copy byte-by-byte.
        while copy_pos < end {
            let byte = match self.buf.get(copy_pos) {
                Some(b) => *b,
                None => break,
            };
            if let Some(dst) = self.buf.get_mut(self.pos) {
                *dst = byte;
            }
            self.pos += 1;
            copy_pos += 1;
        }

        self.buf.allow_resize = true;

        let mut found_nul =
            self.pos > 0 && self.buf.get(self.pos - 1).is_some_and(|b| *b == b'\x00');
        if found_nul {
            self.copy_shared()?;
        }

        let mut got_input = false;
        loop {
            match self.read_to_nul()? {
                None | Some(false) => break,
                Some(true) => {
                    got_input = true;
                }
            }

            self.buf.allow_resize = !found_nul;
            found_nul = true;

            let diff = isize::from(self.decode_prefix_diff()?);
            self.shared_len = self
                .shared_len
                .checked_add(diff)
                .ok_or(Error::SharedOverflow {
                    shared_len: self.shared_len,
                    diff,
                })?;

            if !self.copy_shared()? {
                break;
            }
        }

        let newline = {
            let Some(view) = self.buf.get(..self.pos) else {
                return if got_input {
                    Err(Error::MissingNewline)
                } else {
                    Ok(&mut [])
                };
            };
            memchr::memrchr(b'\n', view)
        };

        match newline {
            Some(newline) => {
                self.partial_entry_start = newline + 1;
                // If no new input was read and the only newlines live before
                // `item_start`, we are at EOF with residual prefix state.
                if !got_input && newline < item_start {
                    return Ok(&mut []);
                }
                match self.buf.get_mut(item_start..self.partial_entry_start) {
                    Some(slice) => Ok(slice),
                    None => Err(Error::MissingNewline),
                }
            }
            None if !got_input => {
                // EOF after a clean entry boundary: residual last-path bytes
                // have no newline, and no new data arrived.
                Ok(&mut [])
            }
            None => Err(Error::MissingNewline),
        }
    }
}

/// Encoder for the frcode format. Writes directly to the underlying `Write`.
///
/// One encoder is typically used per package. On drop / [`finish`](Self::finish)
/// it emits a footer entry that resets the shared-prefix length to zero so the
/// next encoder can start cleanly.
pub struct Encoder<W: Write> {
    writer: W,
    last: Vec<u8>,
    shared_len: i16,
    footer_meta: Vec<u8>,
    footer_path: Vec<u8>,
    footer_written: bool,
}

impl<W: Write> Drop for Encoder<W> {
    fn drop(&mut self) {
        // Best-effort footer write; finish() should be preferred so errors surface.
        let _ = self.write_footer();
    }
}

impl<W: Write> Encoder<W> {
    /// Construct a new encoder that ends with the given footer entry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ForbiddenByte`] when the footer contains NUL or newline.
    pub fn new(writer: W, footer_meta: Vec<u8>, footer_path: Vec<u8>) -> Result<Self> {
        validate_bytes(&footer_meta)?;
        validate_bytes(&footer_path)?;
        Ok(Self {
            writer,
            last: Vec::new(),
            shared_len: 0,
            footer_meta,
            footer_path,
            footer_written: false,
        })
    }

    fn encode_diff(&mut self, diff: i16) -> io::Result<()> {
        let low = (diff & 0xFF) as u8;
        if diff.abs() < i16::from(i8::MAX) {
            self.writer.write_all(&[low])?;
        } else {
            let high = ((diff >> 8) & 0xFF) as u8;
            self.writer.write_all(&[0x80, high, low])?;
        }
        Ok(())
    }

    /// Append metadata bytes for the current entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the metadata contains forbidden bytes or I/O fails.
    pub fn write_meta(&mut self, meta: &[u8]) -> Result<()> {
        validate_bytes(meta)?;
        self.writer.write_all(meta)?;
        Ok(())
    }

    /// Finalize the current entry by writing its path and a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns an error when the path contains forbidden bytes or I/O fails.
    pub fn write_path(&mut self, path: Vec<u8>) -> Result<()> {
        validate_bytes(&path)?;
        self.writer.write_all(b"\x00")?;

        let mut shared: isize = 0;
        let max_shared = i16::MAX as isize;
        for (a, b) in self.last.iter().zip(path.iter()) {
            if a != b || shared > max_shared {
                break;
            }
            shared += 1;
        }
        let shared = shared as i16;

        let diff = shared - self.shared_len;
        self.encode_diff(diff)?;

        self.last = path;
        self.shared_len = shared;

        let pos = shared as usize;
        if let Some(rest) = self.last.get(pos..) {
            self.writer.write_all(rest)?;
        }
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn write_footer(&mut self) -> Result<()> {
        if self.footer_written {
            return Ok(());
        }

        let diff = -self.shared_len;
        self.writer.write_all(&self.footer_meta)?;
        self.writer.write_all(b"\x00")?;
        self.encode_diff(diff)?;
        self.writer.write_all(&self.footer_path)?;
        self.writer.write_all(b"\n")?;
        self.footer_written = true;
        Ok(())
    }

    /// Finish the encoder by writing the footer entry.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the footer cannot be written.
    pub fn finish(mut self) -> Result<()> {
        self.write_footer()
    }
}

fn validate_bytes(data: &[u8]) -> Result<()> {
    if data.contains(&b'\x00') || data.contains(&b'\n') {
        Err(Error::ForbiddenByte)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn encode_paths(paths: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = Encoder::new(&mut out, b"p".to_vec(), b"{}".to_vec()).expect("encoder");
            for path in paths {
                enc.write_meta(b"1r").expect("meta");
                enc.write_path(path.to_vec()).expect("path");
            }
            enc.finish().expect("finish");
        }
        out
    }

    fn decode_all(data: &[u8]) -> Vec<u8> {
        let mut dec = Decoder::new(Cursor::new(data));
        let mut all = Vec::new();
        loop {
            let block = dec.decode().expect("decode");
            if block.is_empty() {
                break;
            }
            all.extend_from_slice(block);
        }
        all
    }

    #[test]
    fn roundtrip_simple_paths() {
        let paths: &[&[u8]] = &[b"/", b"/bin", b"/bin/hello", b"/bin/hi", b"/lib"];
        let encoded = encode_paths(paths);
        let decoded = decode_all(&encoded);

        // Decoded lines are metadata\0path; last line is package footer p\0{}
        let lines: Vec<&[u8]> = decoded
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), paths.len() + 1);
        for (i, path) in paths.iter().enumerate() {
            let line = *lines.get(i).expect("line");
            let sep = memchr::memchr(b'\0', line).expect("nul");
            assert_eq!(line.get(..sep), Some(b"1r".as_slice()));
            assert_eq!(line.get(sep + 1..), Some(*path));
        }
        assert_eq!(lines.get(paths.len()).copied(), Some(b"p\0{}".as_slice()));
    }

    #[test]
    fn encode_diff_short_and_long() {
        // Force a large shared-prefix jump by encoding long identical prefixes.
        let long_a = vec![b'a'; 200];
        let mut long_b = long_a.clone();
        long_b.push(b'b');
        let long_c = vec![b'c'; 10];

        let mut out = Vec::new();
        {
            let mut enc = Encoder::new(&mut out, b"p".to_vec(), b"{}".to_vec()).expect("encoder");
            enc.write_meta(b"d").unwrap();
            enc.write_path(long_a).unwrap();
            enc.write_meta(b"d").unwrap();
            enc.write_path(long_b).unwrap();
            enc.write_meta(b"d").unwrap();
            enc.write_path(long_c).unwrap();
            enc.finish().unwrap();
        }
        let decoded = decode_all(&out);
        assert!(decoded.contains(&b'a'));
        assert!(decoded.contains(&b'b'));
        assert!(decoded.contains(&b'c'));
    }
}
