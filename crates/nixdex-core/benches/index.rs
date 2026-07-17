#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use nixdex_core::database::{Reader, Writer};
use nixdex_core::files::FileTree;
use nixdex_core::store_path::{Origin, StorePath};

fn make_package_tree() -> FileTree {
    FileTree::directory(vec![(
        Bytes::from_static(b"bin"),
        FileTree::directory(vec![(
            Bytes::from_static(b"program"),
            FileTree::regular(1024, true),
        )]),
    )])
}

fn make_dataset(count: usize) -> Vec<StorePath> {
    (0..count)
        .map(|i| {
            StorePath::new(
                "/nix/store".into(),
                format!("{:032x}", i),
                format!("package-{i}"),
                Origin {
                    attr: format!("package-{i}"),
                    output: "out".into(),
                    toplevel: true,
                    system: Some("x86_64-linux".into()),
                },
            )
        })
        .collect()
}

fn build_database(path: &std::path::Path, paths: &[StorePath]) -> u64 {
    let tree = make_package_tree();
    let mut writer = Writer::create(path, 1).expect("create writer");
    for path in paths {
        writer.add(path, &tree, b"").expect("add package");
    }
    writer.finish().expect("finish writer")
}

fn bench_writer_finish(c: &mut Criterion) {
    let mut group = c.benchmark_group("writer_finish");
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("files");

    for count in [100, 1000, 10000] {
        let paths = make_dataset(count);
        group.bench_with_input(BenchmarkId::from_parameter(count), &paths, |b, paths| {
            b.iter(|| {
                let size = build_database(&db_path, paths);
                black_box(size);
            })
        });
    }

    group.finish();
}

fn bench_reader_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("reader_open");

    for count in [100, 1000, 10000] {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("files");
        let paths = make_dataset(count);
        build_database(&db_path, &paths);

        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &db_path,
            |b, db_path| {
                b.iter(|| {
                    let reader = Reader::open(db_path).expect("open reader");
                    black_box(reader);
                })
            },
        );
    }

    group.finish();
}

fn bench_reader_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("reader_search");
    let re = regex::bytes::Regex::new("program").expect("regex");

    for count in [100, 1000, 10000] {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("files");
        let paths = make_dataset(count);
        build_database(&db_path, &paths);
        let reader = Reader::open(&db_path).expect("open reader");

        group.bench_with_input(BenchmarkId::from_parameter(count), &reader, |b, reader| {
            b.iter(|| {
                let hits = reader
                    .search_entries(&re, None, None, None, None)
                    .expect("search");
                black_box(hits);
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_writer_finish,
    bench_reader_open,
    bench_reader_search
);
criterion_main!(benches);
