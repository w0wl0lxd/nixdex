#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use nixdex_core::entry_index::{EntryIndex, EntryIndexBuilder};
use nixdex_core::files::{FileNode, FileTreeEntry};
use nixdex_core::store_path::{Origin, StorePath};

const COUNTS: [usize; 3] = [100, 1_000, 5_000];
const FILES_PER_PACKAGE: usize = 10;

fn make_store_path(i: usize) -> StorePath {
    StorePath::new(
        String::from("/nix/store"),
        format!("{i:032x}"),
        format!("pkg{i}"),
        Origin {
            attr: format!("pkg{i}"),
            output: String::from("out"),
            toplevel: true,
            system: Some(String::from("x86_64-linux")),
        },
    )
}

fn make_entries(pkg: usize, count: usize) -> Vec<FileTreeEntry> {
    (0..FILES_PER_PACKAGE)
        .map(|file| {
            let name = if file == 0 && pkg == count / 2 {
                String::from("ls")
            } else {
                format!("cmd{file}")
            };
            FileTreeEntry {
                path: format!("/nix/store/{pkg:032x}-pkg{pkg}/bin/{name}").into_bytes(),
                node: FileNode::Regular {
                    size: 0,
                    executable: file % 10 == 0,
                },
            }
        })
        .collect()
}

fn make_dataset(count: usize) -> Vec<(StorePath, Vec<FileTreeEntry>)> {
    (0..count)
        .map(|i| (make_store_path(i), make_entries(i, count)))
        .collect()
}

fn build_index(dir: &std::path::Path, data: &[(StorePath, Vec<FileTreeEntry>)]) {
    let mut builder = EntryIndexBuilder::new();
    for (store_path, entries) in data {
        builder.record_package(store_path, entries).unwrap();
    }
    builder.write_sidecars(dir).unwrap();
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("entry_index_build");
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
    let mut group = c.benchmark_group("entry_index_open");
    group.sample_size(50);
    for count in COUNTS {
        let data = make_dataset(count);
        let temp = tempfile::tempdir().unwrap();
        build_index(temp.path(), &data);

        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), temp.path(), |b, dir| {
            b.iter(|| {
                let index = EntryIndex::open(black_box(dir)).unwrap();
                black_box(index);
            });
        });
    }
    group.finish();
}

fn bench_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("entry_index_lookup");
    group.sample_size(50);
    for count in COUNTS {
        let data = make_dataset(count);
        let temp = tempfile::tempdir().unwrap();
        build_index(temp.path(), &data);
        let index = EntryIndex::open(temp.path()).unwrap();

        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &index, |b, index| {
            b.iter(|| {
                let hits = index.lookup_entries(black_box(b"ls")).unwrap();
                black_box(hits);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_build, bench_open, bench_lookup);
criterion_main!(benches);
