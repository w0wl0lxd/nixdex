#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use nixdex_core::basename_index::{BasenameIndex, BasenameIndexBuilder};

const COUNTS: [usize; 3] = [100, 1_000, 10_000];
const FILES_PER_PACKAGE: usize = 10;

fn make_builder(count: usize) -> BasenameIndexBuilder {
    let mut builder = BasenameIndexBuilder::new();
    for pkg in 0..count {
        let label = format!("pkg{pkg}.out");
        let paths: Vec<Vec<u8>> = (0..FILES_PER_PACKAGE)
            .map(|file| format!("/nix/store/{pkg:032x}-pkg{pkg}/bin/cmd{file}").into_bytes())
            .collect();
        builder.record_package(label, paths).unwrap();
    }
    builder
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("basename_index_build");
    group.sample_size(30);
    for count in COUNTS {
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let builder = make_builder(black_box(count));
                black_box(builder);
            });
        });
    }
    group.finish();
}

fn bench_write_and_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("basename_index_write_open");
    group.sample_size(30);
    for count in COUNTS {
        let builder = make_builder(count);
        let temp = tempfile::tempdir().unwrap();
        builder.write_sidecars(temp.path()).unwrap();

        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), temp.path(), |b, dir| {
            b.iter(|| {
                let index = BasenameIndex::open(black_box(dir)).unwrap();
                black_box(index);
            });
        });
    }
    group.finish();
}

fn bench_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("basename_index_lookup");
    group.sample_size(50);
    for count in COUNTS {
        let builder = make_builder(count);
        let temp = tempfile::tempdir().unwrap();
        builder.write_sidecars(temp.path()).unwrap();
        let index = BasenameIndex::open(temp.path()).unwrap();

        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &index, |b, index| {
            b.iter(|| {
                let hits = index.lookup_basename_ordinals(black_box(b"cmd5")).unwrap();
                black_box(hits);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_build, bench_write_and_open, bench_lookup);
criterion_main!(benches);
