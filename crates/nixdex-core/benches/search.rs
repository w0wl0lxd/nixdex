use criterion::{Criterion, black_box, criterion_group, criterion_main};
use nixdex_core::package_search::{SearchDb, SearchField, SearchSort};
use std::io::Write;

fn build_fixture(count: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    for i in 0..count {
        let record = format!(
            r#"{{"attr":"pkg{i}","name":"pkg-{i}","description":"A package named pkg{i}","mainProgram":null}}"#,
        );
        writeln!(buf, "{record}").unwrap();
    }
    buf
}

fn bench_search(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("packages.json");
    let fixture = build_fixture(10_000);
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&fixture)
        .unwrap();

    let db = SearchDb::open(&path).unwrap();

    c.bench_function("search literal attr", |b| {
        b.iter(|| {
            black_box(
                db.search(
                    black_box("pkg5000"),
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

    c.bench_function("search substring description", |b| {
        b.iter(|| {
            black_box(
                db.search(
                    black_box("named pkg5000"),
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

    c.bench_function("search regex attr", |b| {
        b.iter(|| {
            black_box(
                db.search(
                    black_box(r"^pkg50.*"),
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

criterion_group!(benches, bench_search);
criterion_main!(benches);
