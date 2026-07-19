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
    // Leak the tempdir so the file stays valid for the benchmark lifetime.
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
        let (path, _fixture) = fixture_path(count);
        let db = SearchDb::open(&path).unwrap();
        let query = format!("pkg{}", count / 2);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &db, |b, db| {
            b.iter(|| {
                black_box(
                    db.search(
                        black_box(&query),
                        false,
                        SearchField::Attr,
                        false,
                        false,
                        SearchSort::None,
                        None,
                    )
                    .unwrap(),
                );
            });
        });
    }
    group.finish();
}

fn bench_search_exact(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_exact");
    group.sample_size(50);
    for count in COUNTS {
        let (path, _fixture) = fixture_path(count);
        let db = SearchDb::open(&path).unwrap();
        let query = format!("pkg{}", count / 2);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &db, |b, db| {
            b.iter(|| {
                black_box(
                    db.search(
                        black_box(&query),
                        false,
                        SearchField::Attr,
                        false,
                        true,
                        SearchSort::None,
                        None,
                    )
                    .unwrap(),
                );
            });
        });
    }
    group.finish();
}

fn bench_search_description(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_description");
    group.sample_size(50);
    for count in COUNTS {
        let (path, _fixture) = fixture_path(count);
        let db = SearchDb::open(&path).unwrap();
        let query = format!("pkg{}", count / 2);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &db, |b, db| {
            b.iter(|| {
                black_box(
                    db.search(
                        black_box(&query),
                        false,
                        SearchField::Both,
                        false,
                        false,
                        SearchSort::None,
                        None,
                    )
                    .unwrap(),
                );
            });
        });
    }
    group.finish();
}

fn bench_search_regex(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_regex");
    group.sample_size(50);
    for count in COUNTS {
        let (path, _fixture) = fixture_path(count);
        let db = SearchDb::open(&path).unwrap();
        let query = format!(r"^pkg{}.*", count / 2);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &db, |b, db| {
            b.iter(|| {
                black_box(
                    db.search(
                        black_box(&query),
                        true,
                        SearchField::Attr,
                        false,
                        false,
                        SearchSort::None,
                        None,
                    )
                    .unwrap(),
                );
            });
        });
    }
    group.finish();
}

fn bench_search_sort(c: &mut Criterion) {
    let count = 10_000;
    let (path, _fixture) = fixture_path(count);
    let db = SearchDb::open(&path).unwrap();

    let mut group = c.benchmark_group("search_sort");
    group.sample_size(50);
    group.throughput(Throughput::Elements(count as u64));
    for sort in [
        SearchSort::None,
        SearchSort::Attr,
        SearchSort::Name,
        SearchSort::MainProgram,
    ] {
        let label = sort.to_string();
        group.bench_with_input(BenchmarkId::new(label, "10k"), &sort, |b, sort| {
            b.iter(|| {
                black_box(
                    db.search(
                        black_box("pkg"),
                        false,
                        SearchField::Attr,
                        false,
                        false,
                        *sort,
                        None,
                    )
                    .unwrap(),
                );
            });
        });
    }
    group.finish();
}

fn bench_search_fuzzy(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_fuzzy");
    group.sample_size(30);
    for count in [1_000, 10_000] {
        let (path, _fixture) = fixture_path(count);
        let db = SearchDb::open(&path).unwrap();
        let query = format!("pkg{}", count / 2);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &db, |b, db| {
            b.iter(|| {
                black_box(
                    db.search_fuzzy(
                        black_box(&query),
                        SearchField::Attr,
                        false,
                        SearchSort::None,
                        None,
                    )
                    .unwrap(),
                );
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_search_db_open,
    bench_search_literal,
    bench_search_exact,
    bench_search_description,
    bench_search_regex,
    bench_search_sort,
    bench_search_fuzzy
);
criterion_main!(benches);
