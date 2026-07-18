#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use regex::bytes::Regex;

use nixdex_core::database::{Reader, Writer};
use nixdex_core::files::FileTree;
use nixdex_core::generate_sidecars;
use nixdex_core::store_path::{Origin, StorePath};

const PACKAGE_COUNTS: [usize; 3] = [100, 1_000, 5_000];
const FILES_PER_PACKAGE: usize = 10;

fn make_package_tree() -> FileTree {
    let mut entries = Vec::with_capacity(FILES_PER_PACKAGE);
    for file in 0..FILES_PER_PACKAGE {
        let name = if file == 0 {
            Bytes::from_static(b"program")
        } else {
            Bytes::from(format!("program{file}"))
        };
        entries.push((name, FileTree::regular(1024, file % 2 == 0)));
    }
    FileTree::directory(vec![(
        Bytes::from_static(b"bin"),
        FileTree::directory(entries),
    )])
}

fn make_dataset(count: usize) -> Vec<StorePath> {
    (0..count)
        .map(|i| {
            StorePath::new(
                "/nix/store".into(),
                format!("{i:032x}"),
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

fn build_database(path: &std::path::Path, paths: &[StorePath]) {
    let tree = make_package_tree();
    let mut writer = Writer::create(path, 1).expect("create writer");
    for path in paths {
        writer.add(path, &tree, b"").expect("add package");
    }
    writer.finish().expect("finish writer");
}

fn bench_writer_finish(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_writer_finish");
    group.sample_size(50);
    for count in PACKAGE_COUNTS {
        let paths = make_dataset(count);
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &paths, |b, paths| {
            let temp = tempfile::tempdir().expect("tempdir");
            let db_path = temp.path().join("files");
            b.iter(|| {
                build_database(black_box(&db_path), black_box(paths));
            });
        });
    }
    group.finish();
}

fn bench_reader_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_reader_open");
    group.sample_size(50);
    for count in PACKAGE_COUNTS {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("files");
        let paths = make_dataset(count);
        build_database(&db_path, &paths);
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &db_path,
            |b, db_path| {
                b.iter(|| {
                    let reader = Reader::open(black_box(db_path)).expect("open reader");
                    black_box(reader);
                });
            },
        );
    }
    group.finish();
}

fn bench_reader_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_reader_search");
    group.sample_size(50);
    let re = Regex::new("program").expect("regex");
    for count in PACKAGE_COUNTS {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("files");
        let paths = make_dataset(count);
        build_database(&db_path, &paths);
        let reader = Reader::open(&db_path).expect("open reader");
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &reader, |b, reader| {
            b.iter(|| {
                let hits = reader
                    .search_entries(black_box(&re), None, None, None, None)
                    .expect("search");
                black_box(hits);
            });
        });
    }
    group.finish();
}

fn bench_generate_sidecars(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_generate_sidecars");
    group.sample_size(30);
    for count in PACKAGE_COUNTS {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("files");
        let paths = make_dataset(count);
        build_database(&db_path, &paths);
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &db_path,
            |b, db_path| {
                b.iter(|| {
                    black_box(generate_sidecars(black_box(db_path)).expect("generate sidecars"));
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_writer_finish,
    bench_reader_open,
    bench_reader_search,
    bench_generate_sidecars
);
criterion_main!(benches);
