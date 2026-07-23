//! CLI integration tests for the `nixdex` / `nix-index` / `nix-locate` binaries.
//!
//! These tests build small synthetic databases in temp directories and exercise
//! the built binary artifacts. They avoid network or `nix-eval-jobs`.

use std::path::PathBuf;
use std::process::Command;

use bytes::Bytes;
use nixdex_core::{FileTree, Origin, PackageMeta, StorePath, database::Writer};

const NIXDEX_EXE: &str = env!("CARGO_BIN_EXE_nixdex");

/// Resolve the path to a sibling binary (`nix-index` / `nix-locate`) built
/// alongside `nixdex` in the same target directory. Derived from
/// `NIXDEX_EXE` rather than a separate `CARGO_BIN_EXE_<name>` constant to
/// avoid any ambiguity around how Cargo names that variable for binary
/// targets whose name contains a hyphen.
fn sibling_exe(name: &str) -> PathBuf {
    let nixdex_path = PathBuf::from(NIXDEX_EXE);
    let dir = nixdex_path.parent().expect("nixdex exe has a parent dir");
    let mut file_name = std::ffi::OsString::from(name);
    if let Some(ext) = nixdex_path.extension() {
        file_name.push(".");
        file_name.push(ext);
    }
    dir.join(file_name)
}

fn nix_index_exe() -> PathBuf {
    sibling_exe("nix-index")
}

fn nix_locate_exe() -> PathBuf {
    sibling_exe("nix-locate")
}

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
        .env("NIXDEX_NO_DAEMON", "1")
        .output()
        .expect("spawn nixdex")
}

/// Run `exe` with `args`, pinning `HOME` to a known directory and clearing
/// environment variables that could otherwise override the computed default
/// database directory (`XDG_CACHE_HOME`, `NIX_INDEX_DATABASE`).
fn run_with_home(
    exe: impl AsRef<std::path::Path>,
    args: &[&str],
    home: &str,
) -> std::process::Output {
    let exe = exe.as_ref();
    Command::new(exe)
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("NIX_INDEX_DATABASE")
        .output()
        .unwrap_or_else(|err| panic!("spawn {} failed: {err}", exe.display()))
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

#[test]
fn nix_index_binary_defaults_to_upstream_cache_dir() {
    let output = run_with_home(
        nix_index_exe(),
        &["--help"],
        "/tmp/nixdex-test-home-nix-index",
    );
    assert!(
        output.status.success(),
        "nix-index --help failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("/tmp/nixdex-test-home-nix-index/.cache/nix-index"),
        "expected upstream-compatible nix-index cache dir as default, got: {stdout}"
    );
    assert!(
        !stdout.contains("/tmp/nixdex-test-home-nix-index/.cache/nixdex"),
        "nix-index must not default to the nixdex cache dir, got: {stdout}"
    );
}

#[test]
fn nix_locate_binary_defaults_to_upstream_cache_dir() {
    let output = run_with_home(
        nix_locate_exe(),
        &["--help"],
        "/tmp/nixdex-test-home-nix-locate",
    );
    assert!(
        output.status.success(),
        "nix-locate --help failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("/tmp/nixdex-test-home-nix-locate/.cache/nix-index"),
        "expected upstream-compatible nix-index cache dir as default, got: {stdout}"
    );
    assert!(
        !stdout.contains("/tmp/nixdex-test-home-nix-locate/.cache/nixdex"),
        "nix-locate must not default to the nixdex cache dir, got: {stdout}"
    );
}

#[test]
fn nixdex_index_subcommand_defaults_to_nixdex_cache_dir() {
    let output = run_with_home(
        NIXDEX_EXE,
        &["index", "--help"],
        "/tmp/nixdex-test-home-nixdex-index",
    );
    assert!(
        output.status.success(),
        "nixdex index --help failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("/tmp/nixdex-test-home-nixdex-index/.cache/nixdex"),
        "expected nixdex cache dir as default for `nixdex index`, got: {stdout}"
    );
}

#[test]
fn nixdex_locate_subcommand_defaults_to_nixdex_cache_dir() {
    let output = run_with_home(
        NIXDEX_EXE,
        &["locate", "--help"],
        "/tmp/nixdex-test-home-nixdex-locate",
    );
    assert!(
        output.status.success(),
        "nixdex locate --help failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("/tmp/nixdex-test-home-nixdex-locate/.cache/nixdex"),
        "expected nixdex cache dir as default for `nixdex locate`, got: {stdout}"
    );
}

#[test]
fn nixdex_search_subcommand_defaults_to_nixdex_cache_dir() {
    let output = run_with_home(
        NIXDEX_EXE,
        &["search", "--help"],
        "/tmp/nixdex-test-home-nixdex-search",
    );
    assert!(
        output.status.success(),
        "nixdex search --help failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("/tmp/nixdex-test-home-nixdex-search/.cache/nixdex"),
        "expected nixdex cache dir as default for `nixdex search`, got: {stdout}"
    );
}

#[test]
fn nix_locate_explicit_db_flag_overrides_default() {
    // Even though the binary computes a default database directory, an
    // explicit `-d` must still take precedence. We assert this by pointing
    // at a nonexistent directory and checking that the resulting error
    // references the explicit path rather than the default.
    let output = Command::new(nix_locate_exe())
        .args(["-d", "/nonexistent-nixdex-test-dir-xyz", "somepattern"])
        .env("HOME", "/tmp/nixdex-test-home-override")
        .env("NIXDEX_NO_DAEMON", "1")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("NIX_INDEX_DATABASE")
        .output()
        .expect("spawn nix-locate");
    assert!(
        !output.status.success(),
        "expected failure for nonexistent database, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/nonexistent-nixdex-test-dir-xyz"),
        "expected explicit db path to be honored in error, got stderr: {stderr}"
    );
}

#[test]
fn nix_index_binary_parses_args_via_command_factory() {
    // Sanity check that the custom `CommandFactory`/`FromArgMatches` wiring in
    // the `nix-index` binary (needed to inject the upstream-compatible
    // default database directory) still produces a working `Args` value that
    // flows through to the existing validation logic in `run()`.
    let output = Command::new(nix_index_exe())
        .args([
            "--small",
            "--filter-prefix",
            "/usr/",
            "-d",
            "/tmp/nixdex-test-smoke",
        ])
        .env("HOME", "/tmp/nixdex-test-home-smoke")
        .env_remove("XDG_CACHE_HOME")
        .output()
        .expect("spawn nix-index");
    assert!(
        !output.status.success(),
        "expected failure for incompatible --small/--filter-prefix, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--small is incompatible with --filter-prefix"),
        "unexpected stderr: {stderr}"
    );
}
