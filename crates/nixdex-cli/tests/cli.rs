//! CLI integration tests for the `nixdex` / `nix-index` / `nix-locate` binaries.
//!
//! These tests build small synthetic databases in temp directories and exercise
//! the built binary artifacts. They avoid network or `nix-eval-jobs`.

use std::path::PathBuf;
use std::process::Command;

use bytes::Bytes;
use nixdex_core::{FileTree, Origin, PackageMeta, StorePath, database::Writer};

const NIXDEX_EXE: &str = env!("CARGO_BIN_EXE_nixdex");

fn make_store_path(attr: &str, output: &str, toplevel: bool, name: &str, hash: &str) -> StorePath {
    StorePath::new(
        "/nix/store".into(),
        hash.into(),
        name.into(),
        Origin {
            attr: attr.into(),
            output: output.into(),
            toplevel,
            system: None,
        },
    )
}

fn make_tree(entries: Vec<(Bytes, FileTree)>) -> FileTree {
    FileTree::directory(entries)
}

fn write_fixture_database(dir: &std::path::Path) -> PathBuf {
    let files = dir.join("files");
    let mut writer = Writer::create(&files, 3).expect("create writer");

    // Top-level `coreutils` provides `/bin/ls` as an executable.
    let coreutils = make_store_path(
        "coreutils",
        "out",
        true,
        "coreutils-9.11",
        "11111111111111111111111111111111",
    );
    let coreutils_tree = make_tree(vec![(
        Bytes::from_static(b"bin"),
        make_tree(vec![(
            Bytes::from_static(b"ls"),
            FileTree::regular(100, true),
        )]),
    )]);
    writer
        .add(&coreutils, &coreutils_tree, b"")
        .expect("add coreutils");

    // Non-top-level `abuild` also provides `/bin/ls` as a symlink.
    let abuild = make_store_path(
        "abuild",
        "out",
        false,
        "abuild-1.0",
        "22222222222222222222222222222222",
    );
    let abuild_tree = make_tree(vec![(
        Bytes::from_static(b"bin"),
        make_tree(vec![(
            Bytes::from_static(b"ls"),
            FileTree::symlink(Bytes::from_static(b"ls")),
        )]),
    )]);
    writer.add(&abuild, &abuild_tree, b"").expect("add abuild");

    // `hello` provides `/bin/hello` and has a mainProgram.
    let hello = make_store_path(
        "hello",
        "out",
        true,
        "hello-2.12.3",
        "33333333333333333333333333333333",
    );
    let hello_tree = make_tree(vec![(
        Bytes::from_static(b"bin"),
        make_tree(vec![(
            Bytes::from_static(b"hello"),
            FileTree::regular(50, true),
        )]),
    )]);
    writer.add(&hello, &hello_tree, b"").expect("add hello");

    writer.finish().expect("finish writer");

    // Write packages.json sidecar for `nixdex search`.
    let packages_json = dir.join("packages.json");
    let mut contents = String::new();
    for meta in [
        PackageMeta {
            attr: "coreutils".into(),
            name: "coreutils-9.11".into(),
            description: Some("GNU Core Utilities".into()),
            main_program: None,
        },
        PackageMeta {
            attr: "abuild".into(),
            name: "abuild-1.0".into(),
            description: Some("Alpine build tools".into()),
            main_program: Some("abuild".into()),
        },
        PackageMeta {
            attr: "hello".into(),
            name: "hello-2.12.3".into(),
            description: Some("A friendly greeting".into()),
            main_program: Some("hello".into()),
        },
    ] {
        contents.push_str(&sonic_rs::to_string(&meta).expect("serialize PackageMeta"));
        contents.push('\n');
    }
    std::fs::write(&packages_json, contents).expect("write packages.json");

    files
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(NIXDEX_EXE)
        .args(args)
        .output()
        .expect("spawn nixdex")
}

#[test]
fn index_rejects_zero_requests() {
    let output = run(&["index", "--requests", "0", "-d", "/tmp/nixdex-test-zero"]);
    assert!(
        !output.status.success(),
        "expected failure for --requests 0, got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requests must be between 1 and 1000"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn which_prefers_toplevel_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_database(dir.path());

    let output = run(&["which", "-d", dir.path().to_str().unwrap(), "ls"]);
    assert!(output.status.success(), "nixdex which failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("");
    assert_eq!(
        first_line, "coreutils.out",
        "toplevel package should be first"
    );
}

#[test]
fn which_all_lists_toplevel_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_database(dir.path());

    let output = run(&["which", "--all", "-d", dir.path().to_str().unwrap(), "ls"]);
    assert!(
        output.status.success(),
        "nixdex which --all failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.first().copied(), Some("coreutils.out"));
    assert!(
        lines.iter().any(|l| l.contains("(abuild.out)")),
        "non-toplevel provider missing"
    );
}

#[test]
fn locate_finds_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_database(dir.path());

    let output = run(&["locate", "-d", dir.path().to_str().unwrap(), "-w", "ls"]);
    assert!(output.status.success(), "nixdex locate failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("coreutils.out"),
        "missing coreutils match: {stdout}"
    );
    assert!(stdout.contains("/bin/ls"), "missing /bin/ls path: {stdout}");
}

#[test]
fn search_main_program_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_database(dir.path());

    let output = run(&[
        "search",
        "-d",
        dir.path().to_str().unwrap(),
        "--field",
        "main-program",
        "hello",
    ]);
    assert!(output.status.success(), "nixdex search failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"), "expected hello match: {stdout}");
    assert!(
        !stdout.contains("coreutils"),
        "coreutils should not match main-program search"
    );
}

#[test]
fn command_not_found_suggests_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_database(dir.path());

    let output = run(&[
        "command-not-found",
        "-d",
        dir.path().to_str().unwrap(),
        "ls",
    ]);
    assert_eq!(
        output.status.code(),
        Some(127),
        "expected exit status 127 for command-not-found"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("coreutils"),
        "expected coreutils suggestion in stderr: {stderr}"
    );
}

#[test]
fn index_rejects_full_and_small_together() {
    let output = run(&[
        "index",
        "--full",
        "--small",
        "-d",
        "/tmp/nixdex-test-full-small",
    ]);
    assert!(
        !output.status.success(),
        "expected failure for --full --small, got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--full") && stderr.contains("--small"),
        "expected conflict message mentioning both flags: {stderr}"
    );
}

#[test]
fn update_rejects_full_and_small_together() {
    let output = run(&[
        "update",
        "--full",
        "--small",
        "-d",
        "/tmp/nixdex-test-update-full-small",
    ]);
    assert!(
        !output.status.success(),
        "expected failure for --full --small, got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--full") && stderr.contains("--small"),
        "expected conflict message mentioning both flags: {stderr}"
    );
}

#[test]
fn daemon_rejects_full_and_small_together() {
    let output = run(&["daemon", "--full", "--small"]);
    assert!(
        !output.status.success(),
        "expected failure for --full --small, got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--full") && stderr.contains("--small"),
        "expected conflict message mentioning both flags: {stderr}"
    );
}

#[test]
fn index_default_small_rejects_custom_filter_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(&[
        "index",
        "-d",
        dir.path().to_str().unwrap(),
        "--filter-prefix",
        "/nix/store",
    ]);
    assert!(
        !output.status.success(),
        "expected failure for default --small with custom --filter-prefix"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--small is incompatible with --filter-prefix"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn which_reports_helpful_error_for_missing_database() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = run(&["which", "-d", dir.path().to_str().unwrap(), "ls"]);
    assert!(
        !output.status.success(),
        "expected failure for missing database"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nix-index") && stderr.contains("nixdex update"),
        "expected helpful hint mentioning nix-index / nixdex update: {stderr}"
    );
}

#[test]
fn generate_sidecars_creates_sidecar_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_database(dir.path());

    // Remove any sidecars produced by finish() to exercise generation.
    for name in [
        "files.basename.fst",
        "files.basename.postings",
        "files.packages.names",
    ] {
        let _ = std::fs::remove_file(dir.path().join(name));
    }

    let output = run(&["generate-sidecars", "-d", dir.path().to_str().unwrap()]);
    assert!(
        output.status.success(),
        "nixdex generate-sidecars failed: {output:?}"
    );
    assert!(dir.path().join("files.basename.fst").is_file());
    assert!(dir.path().join("files.basename.postings").is_file());
    assert!(dir.path().join("files.packages.names").is_file());
}
