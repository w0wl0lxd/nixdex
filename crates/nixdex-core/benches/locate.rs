use bytes::Bytes;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use regex::bytes::Regex;

use nixdex_core::database::Reader;
use nixdex_core::files::FileTree;
use nixdex_core::store_path::{Origin, StorePath};

const DB_ENV_VAR: &str = "NIXDEX_BENCH_DB";

fn build_synthetic_db(path: &std::path::Path) {
    let mut writer =
        nixdex_core::database::Writer::create(path, 3).expect("create benchmark database");

    for pkg in 0..1_000 {
        let name = format!("pkg{pkg}-1.0");
        let hash = format!("{pkg:032x}");
        let store_path = StorePath::new(
            String::from("/nix/store"),
            hash,
            name,
            Origin {
                attr: format!("pkg{pkg}"),
                output: String::from("out"),
                toplevel: true,
                system: Some(String::from("x86_64-linux")),
            },
        );

        let mut bin_entries = Vec::with_capacity(100);
        for file in 0..100 {
            let exe = file % 10 == 0;
            bin_entries.push((Bytes::from(format!("cmd{file}")), FileTree::regular(0, exe)));
        }
        // Ensure a predictable "bin/ls" target for the search benchmark.
        if pkg == 500 {
            bin_entries.push((Bytes::from_static(b"ls"), FileTree::regular(0, true)));
        }

        let tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(bin_entries),
        )]);
        writer.add(&store_path, &tree, b"").expect("add package");
    }

    writer.finish().expect("finish database");
}

fn db_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var(DB_ENV_VAR) {
        return std::path::PathBuf::from(path);
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("files");
    build_synthetic_db(&path);
    // Leak the tempdir so the file remains valid for the benchmark lifetime.
    Box::leak(Box::new(dir));
    path
}

fn bench_locate_open(c: &mut Criterion) {
    let path = db_path();
    c.bench_function("locate open (cold-ish)", |b| {
        b.iter(|| {
            let reader = Reader::open(&path).expect("open database");
            black_box(reader);
        });
    });
}

fn bench_locate_search(c: &mut Criterion) {
    let path = db_path();
    let reader = Reader::open(&path).expect("open database");
    let pattern = Regex::new("bin/ls").expect("regex");

    c.bench_function("locate search warm", |b| {
        b.iter(|| {
            let hits = reader
                .search_entries(&pattern, None, None, None, None)
                .expect("search");
            black_box(hits);
        });
    });
}

criterion_group!(benches, bench_locate_open, bench_locate_search);
criterion_main!(benches);
