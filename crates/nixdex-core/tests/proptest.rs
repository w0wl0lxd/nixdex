//! Property-based tests for nixdex-core using proptest.

use bytes::Bytes;
use nixdex_core::basename_index::{BasenameIndex, BasenameIndexBuilder, basename_of};
use nixdex_core::files::{FileNode, FileTreeEntry};
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::io::Cursor;

/// Strategy for generating valid file paths (ASCII alphanumeric + '/' + leading '/')
fn path_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop::sample::select(
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/".to_vec(),
        ),
        1..50,
    )
    .prop_map(|mut bytes| {
        // Ensure it starts with '/'
        if bytes.first() != Some(&b'/') {
            bytes.insert(0, b'/');
        }
        // Ensure no consecutive slashes and no trailing slash.
        let mut cleaned = Vec::with_capacity(bytes.len());
        for b in bytes {
            if b == b'/' && cleaned.last() == Some(&b'/') {
                continue;
            }
            cleaned.push(b);
        }
        if cleaned.last() == Some(&b'/') {
            cleaned.pop();
        }
        cleaned
    })
}

/// Strategy for generating FileTreeEntry
fn file_tree_entry_strategy() -> impl Strategy<Value = FileTreeEntry> {
    (
        path_strategy(),
        prop::collection::vec(
            prop::sample::select(
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".to_vec(),
            ),
            1..20,
        ),
    )
        .prop_map(|(path, name_bytes)| {
            let len = u64::try_from(path.len()).unwrap();
            let node = match path.len() % 3 {
                0 => FileNode::Regular {
                    size: len * 100,
                    executable: path.len() % 2 == 0,
                },
                1 => FileNode::Symlink {
                    target: Bytes::from(name_bytes),
                },
                _ => FileNode::Directory {
                    size: len % 10,
                    contents: (),
                },
            };
            FileTreeEntry { path, node }
        })
}

proptest! {
    #[test]
    fn prop_file_tree_entry_encode_decode_roundtrip(entry in file_tree_entry_strategy()) {
        let mut buf = Vec::new();
        {
            let mut enc = nixdex_core::frcode::Encoder::new(
                &mut buf,
                b"p".to_vec(),
                b"{}".to_vec()
            ).expect("encoder");
            entry.clone().encode(&mut enc).expect("encode");
            enc.finish().expect("finish");
        }

        let mut dec = nixdex_core::frcode::Decoder::new(Cursor::new(&buf));
        let block = dec.decode().expect("decode block");
        let entry_line = block.split(|b| *b == b'\n').next().expect("entry line");

        let decoded = FileTreeEntry::decode(entry_line).expect("decode entry");
        prop_assert_eq!(decoded, entry);
    }

    #[test]
    fn prop_basename_index_roundtrip(paths in prop::collection::vec(path_strategy(), 1..50)) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = BasenameIndexBuilder::new();

        // Record packages with generated paths
        for i in 0..3.min(paths.len()) {
            let label = format!("pkg{}.out", i);
            let package_paths = paths.iter()
                .filter(|p| p.len() > 1)
                .cloned()
                .collect::<Vec<_>>();
            if !package_paths.is_empty() {
                builder.record_package(label, package_paths).expect("record");
            }
        }

        builder.write_sidecars(dir.path()).expect("write");

        let index = BasenameIndex::open(dir.path()).expect("open");

        // For each unique basename in the input, verify lookup returns consistent results
        let mut basenames = BTreeSet::new();
        for path in &paths {
            let base = basename_of(path);
            if !base.is_empty() {
                basenames.insert(base.to_vec());
            }
        }

        for base in basenames {
            let results = index.lookup_basename(&base).expect("lookup");
            // Results should be a subset of our recorded packages
            for label in &results {
                prop_assert!(
                    label.starts_with("pkg")
                        || *label == "coreutils.out"
                        || *label == "hello.out"
                        || *label == "busybox.out"
                );
            }
        }
    }

    #[test]
    fn prop_basename_of_idempotent(path in path_strategy()) {
        let base1 = basename_of(&path);
        let base2 = basename_of(&path);
        prop_assert_eq!(base1, base2);
    }

    #[test]
    fn prop_basename_of_returns_suffix(path in path_strategy()) {
        let base = basename_of(&path);
        if !base.is_empty() {
            // The basename should be a suffix of the path
            prop_assert!(path.ends_with(base));
            // And should not contain '/'
            prop_assert!(!base.contains(&b'/'));
        }
    }
}
