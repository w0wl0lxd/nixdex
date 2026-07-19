#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use regex::bytes::Regex;

use nixdex_core::database::{Reader, SearchMode, SearchOptions, SearchSort};
use nixdex_core::files::FileTree;
use nixdex_core::generate_sidecars;
use nixdex_core::store_path::{Origin, StorePath};

const DB_ENV_VAR: &str = "NIXDEX_BENCH_DB";

const PACKAGE_COUNTS: [usize; 3] = [100, 1_000, 5_000];
const FILES_PER_PACKAGE: usize = 10;

fn build_synthetic_db(path: &std::path::Path, packages: usize) {
    let mut writer =
        nixdex_core::database::Writer::create(path, 3).expect("create benchmark database");

    for pkg in 0..packages {
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

        let mut bin_entries = Vec::with_capacity(FILES_PER_PACKAGE);
        for file in 0..FILES_PER_PACKAGE {
            let exe = file % 10 == 0;
            let name = if file == 0 && pkg == packages / 2 {
                Bytes::from_static(b"ls")
            } else {
                Bytes::from(format!("cmd{file}"))
            };
            bin_entries.push((name, FileTree::regular(0, exe)));
        }

        let tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(bin_entries),
        )]);
        writer.add(&store_path, &tree, b"").expect("add package");
    }

    writer.finish().expect("finish database");
}

fn db_path(packages: usize) -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("files");
    build_synthetic_db(&path, packages);
    generate_sidecars(&path).expect("generate sidecars");
    // Leak the tempdir so the file remains valid for the benchmark lifetime.
    Box::leak(Box::new(dir));
    path
}

fn real_db_path() -> Option<std::path::PathBuf> {
    std::env::var(DB_ENV_VAR).ok().map(std::path::PathBuf::from)
}

fn db_files_path(packages: usize) -> std::path::PathBuf {
    real_db_path().unwrap_or_else(|| db_path(packages))
}

fn db_dir_for(packages: usize) -> std::path::PathBuf {
    db_files_path(packages).parent().unwrap().to_path_buf()
}

fn open_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("locate_open");
    group.sample_size(50);
    if let Some(path) = real_db_path() {
        group.bench_function("real db", |b| {
            b.iter(|| {
                let reader = Reader::open(black_box(&path)).expect("open database");
                black_box(reader);
            });
        });
    } else {
        for count in PACKAGE_COUNTS {
            let path = db_path(count);
            group.throughput(Throughput::Elements(count as u64));
            group.bench_with_input(BenchmarkId::from_parameter(count), &path, |b, path| {
                b.iter(|| {
                    let reader = Reader::open(black_box(path)).expect("open database");
                    black_box(reader);
                });
            });
        }
    }
    group.finish();
}

fn search_entries_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("locate_search_entries");
    group.sample_size(50);
    let patterns: [(&str, &str); 4] = [
        ("literal bin/ls", r"bin/ls"),
        ("regex bin/.*", r"bin/.*"),
        ("regex cmd5", r"cmd5"),
        ("regex cmd5$", r"cmd5$"),
    ];

    for count in PACKAGE_COUNTS {
        let path = db_files_path(count);
        let reader = Reader::open(&path).expect("open database");
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        for (label, pattern) in patterns {
            let re = Regex::new(pattern).expect("valid regex");
            group.bench_with_input(
                BenchmarkId::new(label, count),
                &(&reader, &re),
                |b, &(reader, re)| {
                    b.iter(|| {
                        let hits = reader
                            .search_entries(black_box(re), None, None, None, None)
                            .expect("search");
                        black_box(hits);
                    });
                },
            );
        }
    }
    group.finish();
}

fn search_results_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("locate_search_results");
    group.sample_size(50);
    let queries: [(&str, &str, Option<&str>); 4] = [
        ("literal bin/ls", r"bin/ls", None),
        ("literal bin/ls exact-basename", r"bin/ls", Some("ls")),
        ("regex bin/.*", r"bin/.*", None),
        ("regex cmd5", r"cmd5", None),
    ];

    for count in PACKAGE_COUNTS {
        let dir = db_dir_for(count);
        group.throughput(Throughput::Elements((count * FILES_PER_PACKAGE) as u64));
        for (label, pattern, exact_basename) in queries {
            let opts = SearchOptions {
                database: dir.clone(),
                pattern: pattern.to_string(),
                hash: None,
                package_pattern: None,
                exact_basename: exact_basename.map(String::from),
                exact_path: None,
                path_prefix: None,
                file_type: &[],
                mode: SearchMode::Minimal,
                json: false,
                limit: None,
                count: false,
                sort: SearchSort::None,
                min_size: None,
                max_size: None,
                exclude_fhs: false,
            };
            group.bench_with_input(BenchmarkId::new(label, count), &opts, |b, opts| {
                b.iter(|| {
                    let hits = nixdex_core::search_database_results(black_box(opts))
                        .expect("search results");
                    black_box(hits);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    open_baseline,
    search_entries_baseline,
    search_results_baseline
);
criterion_main!(benches);
