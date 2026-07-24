#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use nixdex_core::package_search::{SearchDb, SearchField, SearchSort};

const COUNTS: [usize; 3] = [1_000, 10_000, 50_000];

fn build_fixture(count: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(count * 96);
    for i in 0..count {
        let record = format!(
            r#"{{"attr":"pkg{i}","name":"pkg-{i}","description":"A package named pkg{i}","mainProgram":null}}"#,
        );
        writeln!(buf, "{record}").unwrap();
    }
    buf
}

fn fixture_path(count: usize) -> (std::path::PathBuf, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("packages.json");
    let fixture = build_fixture(count);
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&fixture)
        .unwrap();
    Box::leak(Box::new(dir));
    (path, fixture)
}

fn bench_search_db_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_db_open");
    group.sample_size(50);
    for count in COUNTS {
        let (path, fixture) = fixture_path(count);
        group.throughput(Throughput::Bytes(fixture.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &path, |b, path| {
            b.iter(|| black_box(SearchDb::open(black_box(path)).unwrap()));
        });
    }
    group.finish();
}

fn bench_search_literal(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_literal");
    group.sample_size(50);
    for count in COUNTS {
        let (path, fixture) = fixture_path(count);
        group.throughput(Throughput::Bytes(fixture.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &path, |b, path| {
            let db = black_box(SearchDb::open(black_box(path)).unwrap());
            b.iter(|| {
                db.search(
                    black_box("pkg-"),
                    black_box(false),
                    black_box(SearchField::Both),
                    black_box(true),
                    black_box(false),
                    black_box(SearchSort::None),
                    black_box(None),
                )
            });
        });
    }
    group.finish();
}

fn bench_search_regex(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_regex");
    group.sample_size(50);
    for count in COUNTS {
        let (path, fixture) = fixture_path(count);
        group.throughput(Throughput::Bytes(fixture.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &path, |b, path| {
            let db = black_box(SearchDb::open(black_box(path)).unwrap());
            b.iter(|| {
                db.search(
                    black_box("^pkg-\\d+$"),
                    black_box(true),
                    black_box(SearchField::Both),
                    black_box(true),
                    black_box(false),
                    black_box(SearchSort::None),
                    black_box(None),
                )
            });
        });
    }
    group.finish();
}

fn bench_search_parallel_trigram(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_parallel_trigram");
    group.sample_size(50);
    for count in COUNTS {
        let (path, fixture) = fixture_path(count);
        group.throughput(Throughput::Bytes(fixture.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &path, |b, path| {
            let db = black_box(SearchDb::open(black_box(path)).unwrap());
            b.iter(|| {
                db.search(
                    black_box("package named"),
                    black_box(false),
                    black_box(SearchField::Description),
                    black_box(true),
                    black_box(false),
                    black_box(SearchSort::None),
                    black_box(None),
                )
            });
        });
    }
    group.finish();
}

fn bench_search_ngram_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_ngram_cache");
    group.sample_size(50);
    for count in COUNTS {
        let (path, fixture) = fixture_path(count);
        group.throughput(Throughput::Bytes(fixture.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &path, |b, path| {
            let db = black_box(SearchDb::open(black_box(path)).unwrap());
            b.iter(|| {
                db.search(
                    black_box("pkg"),
                    black_box(false),
                    black_box(SearchField::Both),
                    black_box(true),
                    black_box(false),
                    black_box(SearchSort::None),
                    black_box(None),
                )
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_search_db_open,
    bench_search_literal,
    bench_search_regex,
    bench_search_parallel_trigram,
    bench_search_ngram_cache,
);
criterion_main!(benches);
