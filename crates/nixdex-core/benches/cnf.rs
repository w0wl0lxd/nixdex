//! Benchmarks for command-not-found (CNF) lookups.
//!
//! The hard requirement is that a CNF lookup — the cold, no-daemon path that
//! opens the command sidecars and resolves a command — stays under a 50 ms
//! cap. The resident path (an already-loaded `CommandIndex`, as the daemon
//! holds) is sub-millisecond.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

use nixdex_core::command_index::CommandIndex;
use nixdex_core::database::Writer;
use nixdex_core::files::FileTree;
use nixdex_core::store_path::{Origin, StorePath};

const DB_ENV_VAR: &str = "NIXDEX_BENCH_DB";
const PACKAGE_COUNT: u32 = 2_000;
const TARGET_COMMAND: &[u8] = b"cmd1000";

static DB: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Build a synthetic database whose packages each provide one command under
/// `/bin` and one under `/usr/bin`, then generate all sidecars (including the
/// command-provider index). The result on disk mirrors a real prebuilt index.
fn build_synthetic_db(path: &std::path::Path) {
    let mut writer = Writer::create(path, 3).expect("create benchmark database");

    for pkg in 0..PACKAGE_COUNT {
        let name = format!("pkg{pkg}-1.0");
        let hash = format!("{pkg:032x}");
        let store_path = StorePath::new(
            String::from("/nix/store"),
            hash,
            name,
            Origin {
                attr: format!("pkg{pkg}"),
                output: String::from("out"),
                toplevel: pkg % 2 == 0,
                system: Some(String::from("x86_64-linux")),
            },
        );

        let tree = FileTree::directory(vec![
            (
                Bytes::from_static(b"bin"),
                FileTree::directory(vec![(
                    Bytes::from(format!("cmd{pkg}")),
                    FileTree::regular(0, true),
                )]),
            ),
            (
                Bytes::from_static(b"usr"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"bin"),
                    FileTree::directory(vec![(
                        Bytes::from(format!("ucmd{pkg}")),
                        FileTree::regular(0, true),
                    )]),
                )]),
            ),
        ]);

        writer.add(&store_path, &tree, b"").expect("add package");
    }

    writer.finish().expect("finish database");
    nixdex_core::generate_sidecars(path).expect("generate sidecars");
}

/// Path to the `files` database, built once and leaked for the bench lifetime.
fn db_path() -> &'static std::path::Path {
    DB.get_or_init(|| {
        if let Ok(path) = std::env::var(DB_ENV_VAR) {
            return std::path::PathBuf::from(path);
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("files");
        build_synthetic_db(&path);
        let _ = Box::leak(Box::new(dir));
        path
    })
}

fn bench_cnf_cold(c: &mut Criterion) {
    let path = db_path();
    let dir = path.parent().expect("db dir");
    c.bench_function("cnf cold open+lookup", |b| {
        b.iter(|| {
            let idx = CommandIndex::open(dir).expect("open command index");
            let providers = idx.lookup_command(TARGET_COMMAND).expect("lookup");
            black_box(providers);
        });
    });
}

fn bench_cnf_resident(c: &mut Criterion) {
    let path = db_path();
    let dir = path.parent().expect("db dir");
    let idx = CommandIndex::open(dir).expect("open command index");
    c.bench_function("cnf resident lookup", |b| {
        b.iter(|| {
            let providers = idx.lookup_command(TARGET_COMMAND).expect("lookup");
            black_box(providers);
        });
    });
}

/// Enforce the <50 ms hard cap on the cold CNF path via a p99 measurement.
fn bench_cnf_cap(_c: &mut Criterion) {
    let path = db_path();
    let dir = path.parent().expect("db dir");

    let iters = 200u32;
    let mut samples = Vec::with_capacity(iters as usize);
    for _ in 0..iters {
        let start = Instant::now();
        let idx = CommandIndex::open(dir).expect("open command index");
        let _ = idx.lookup_command(TARGET_COMMAND).expect("lookup");
        samples.push(start.elapsed());
    }

    samples.sort_unstable();
    let p99_idx = (samples.len() * 99 / 100).min(samples.len().saturating_sub(1));
    let p99 = samples[p99_idx];
    let n = u32::try_from(samples.len()).expect("sample count fits u32");
    let mean = samples.iter().sum::<Duration>() / n;

    eprintln!("cnf cold: mean={mean:?} p99={p99:?}");
    assert!(
        p99 < Duration::from_millis(50),
        "cnf cold p99 {p99:?} exceeds the 50 ms cap"
    );
}

criterion_group!(benches, bench_cnf_cold, bench_cnf_resident, bench_cnf_cap);
criterion_main!(benches);
