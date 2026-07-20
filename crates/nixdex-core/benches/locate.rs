use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use regex::bytes::Regex;

use nixdex_core::database::{Reader, SearchMode, SearchOptions, SearchSort, search_results};
use nixdex_core::entry_index::EntryIndex;
use nixdex_core::files::{FileTree, FileType};
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

fn open_baseline(c: &mut Criterion) {
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

fn bench_locate_entry_lookup(c: &mut Criterion) {
    let path = db_path();
    let dir = path.parent().expect("db dir");
    nixdex_core::database::generate_sidecars(&path).expect("generate sidecars");

    let index = EntryIndex::open(dir).expect("open entry index");

    c.bench_function("locate entry lookup (basename->store paths)", |b| {
        b.iter(|| {
            let entries = index.lookup_entries(b"ls").expect("lookup");
            black_box(entries);
        });
    });
}

fn bench_locate_ngram_search(c: &mut Criterion) {
    let path = db_path();
    let dir = path.parent().expect("db dir");
    nixdex_core::database::generate_sidecars(&path).expect("generate sidecars");

    let options = SearchOptions {
        database: dir.to_path_buf(),
        pattern: String::from("bin/ls"),
        hash: None,
        package_pattern: None,
        exact_basename: None,
        exact_path: None,
        path_prefix: None,
        literal_pattern: Some(String::from("bin/ls")),
        file_type: &[FileType::Regular { executable: false }],
        mode: SearchMode::Minimal,
        json: false,
        limit: None,
        count: false,
        sort: SearchSort::None,
        min_size: None,
        max_size: None,
        exclude_fhs: false,
    };

    c.bench_function("locate search via ngram index", |b| {
        b.iter(|| {
            let hits = search_results(&options).expect("search");
            black_box(hits);
        });
    });
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

    let path = db_path();
    let reader = Reader::open(&path).expect("open database");
    for (label, pattern) in patterns {
        let re = Regex::new(pattern).expect("valid regex");
        group.bench_with_input(
            BenchmarkId::new(label, 1000),
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

    let path = db_path();
    let dir = path.parent().unwrap().to_path_buf();
    for (label, pattern, exact_basename) in queries {
        let opts = SearchOptions {
            database: dir.clone(),
            pattern: pattern.to_string(),
            hash: None,
            package_pattern: None,
            literal_pattern: None,
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
        group.bench_with_input(BenchmarkId::new(label, 1000), &opts, |b, opts| {
            b.iter(|| {
                let hits =
                    nixdex_core::search_database_results(black_box(opts)).expect("search results");
                black_box(hits);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    open_baseline,
    bench_locate_search,
    bench_locate_entry_lookup,
    bench_locate_ngram_search,
    search_entries_baseline,
    search_results_baseline
);
criterion_main!(benches);
