#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use nixdex_core::ngram_index::{NgramIndex, NgramIndexBuilder};

const COUNTS: [usize; 3] = [100, 1_000, 5_000];
const FILES_PER_PACKAGE: usize = 10;

fn make_paths(pkg: usize, count: usize) -> Vec<Vec<u8>> {
    (0..FILES_PER_PACKAGE)
        .map(|file| {
            let name = if file == 0 && pkg == count / 2 {
                "ls"
            } else {
                "cmd"
            };
            format!("/nix/store/{pkg:032x}-pkg{pkg}/bin/{name}{file}").into_bytes()
        })
        .collect()
}

fn make_dataset(count: usize) -> Vec<Vec<Vec<u8>>> {
    (0..count).map(|i| make_paths(i, count)).collect()
}

fn build_index(dir: &std::path::Path, data: &[Vec<Vec<u8>>]) {
    let mut builder = NgramIndexBuilder::new();
    for (ordinal, paths) in data.iter().enumerate() {
        builder
            .record_package(u32::try_from(ordinal).unwrap(), paths.iter().cloned())
            .unwrap();
    }
    builder.write_sidecars(dir).unwrap();
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("ngram_index_build");
    group.sample_size(30);
    for count in [100, 1_000] {
        let data = make_dataset(count);
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &data, |b, data| {
            let temp = tempfile::tempdir().unwrap();
            b.iter(|| {
                build_index(black_box(temp.path()), black_box(data));
            });
        });
    }
    group.finish();
}

fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("ngram_index_open");
    group.sample_size(50);
    for count in COUNTS {
        let data = make_dataset(count);
        let temp = tempfile::tempdir().unwrap();
        build_index(temp.path(), &data);

        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), temp.path(), |b, dir| {
            b.iter(|| {
                let index = NgramIndex::open(black_box(dir)).unwrap();
                black_box(index);
            });
        });
    }
    group.finish();
}

fn bench_candidates(c: &mut Criterion) {
    let mut group = c.benchmark_group("ngram_index_candidates");
    group.sample_size(50);
    for count in COUNTS {
        let data = make_dataset(count);
        let temp = tempfile::tempdir().unwrap();
        build_index(temp.path(), &data);
        let index = NgramIndex::open(temp.path()).unwrap();

        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &index, |b, index| {
            b.iter(|| {
                let hits = index.candidate_ordinals(black_box("bin/ls")).unwrap();
                black_box(hits);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_build, bench_open, bench_candidates);
criterion_main!(benches);
