# Upstream `nix-index` baseline

Upstream source: `/home/w0w/dev/nixdex/.upstream/nix-index` (nix-community/nix-index, `Cargo.toml` version `0.1.12`).

## What was built/run

- `cargo build` (debug) in the upstream repo: succeeded with `nix-shell -p sqlite pkg-config` (system `sqlite3` is required by `rusqlite`); `target/debug/nix-index` and `target/debug/nix-locate` were produced.
- `cargo build --release` also produced `target/release/nix-index` and `target/release/nix-locate`.
- `nix run nixpkgs#nix-index` / `nix shell nixpkgs#nix-index -c nix-locate` were tested; the prebuilt nixpkgs binary is older (`0.1.10`) and **could not parse zstd-compressed `.ls` cache responses**, so the local `0.1.12` build was used for the database.

## CLI flags and defaults

### `nix-index` (built from `src/bin/nix-index.rs`, lines 135-184)

```
Nix (package manager) indexing primitives

Usage: nix-index [OPTIONS]

Options:
  -r, --requests <JOBS>
          Make REQUESTS http requests in parallel
          [default: 100]

  -d, --db <DATABASE>
          Directory where the index is stored
          [env: NIX_INDEX_DATABASE=]
          [default: /home/w0w/.cache/nix-index/]

  -f, --nixpkgs <NIXPKGS>
          Path to nixpkgs for which to build the index, as accepted by nix-env -f
          [default: <nixpkgs>]

  -s, --system <platform>
          Specify system platform for which to build the index, accepted by nix-env --argstr system

  -c, --compression <COMPRESSION_LEVEL>
          Zstandard compression level
          [default: 22]

      --show-trace
          Show a stack trace in the case of a Nix evaluation error

      --filter-prefix <FILTER_PREFIX>
          Only add paths starting with PREFIX (e.g. `/bin/`)
          [default: ""]

      --path-cache
          Store and load results of fetch phase in a file called paths.cache. This speeds up testing
          different database formats / compression.
          Note: does not check if the cached data is up to date! Use only for development.

      --extra-scopes <EXTRA_SCOPES>
          [default: haskellPackages rPackages coqPackages texlive.pkgs]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

Key defaults:
- HTTP parallelism: `100` (`-r`, `--requests`)
- DB directory: `~/.cache/nix-index/` (`-d`, `--db`, env `NIX_INDEX_DATABASE`)
- nixpkgs: `<nixpkgs>` (`-f`, `--nixpkgs`)
- zstd compression: `22` (`-c`, `--compression`)
- `--filter-prefix` empty, so all files are indexed unless given
- `--extra-scopes` defaults to `haskellPackages rPackages coqPackages texlive.pkgs`

### `nix-locate` (built from `src/bin/nix-locate.rs`, lines 262-323)

```
Nix (package manager) indexing primitives

Usage: nix-locate [OPTIONS] <PATTERN>

Arguments:
  <PATTERN>  Pattern for which to search

Options:
  -d, --db <DATABASE>      Directory where the index is stored [env: NIX_INDEX_DATABASE=] [default: /home/w0w/.cache/nix-index/]
  -r, --regex              Treat PATTERN as regex instead of literal text. Also applies to NAME
  -p, --package <PACKAGE>  Only print matches from packages whose name matches PACKAGE
      --hash <HASH>        Only print matches from the package that has the given HASH
      --all                Print all matches, not only print from packages that show up in `nix-env -qa`
  -t, --type <TYPE>        Only print matches for files that have this type. If the option is given multiple times, a file will be printed if it has any of the given types. [options: (r)egular file, e(x)cutable, (d)irectory, (s)ymlink] [possible values: r, x, d, s]
      --no-group           Disables grouping of paths with the same matching part. By default, a path will only be printed if the pattern matches some part of the last component of the path. For example, the pattern `a/foo` would match all of `a/foo`, `a/foo/some_file` and `a/foo/another_file`, but only the first match will be printed. This option disables that behavior and prints all matches
      --color <COLOR>      Whether to use colors in output. If auto, only use colors if outputting to a terminal [default: auto] [possible values: always, never, auto]
  -w, --whole-name         Only print matches for files or directories whose basename matches PATTERN exactly. This means that the pattern `bin/foo` will only match a file called `bin/foo` or `xx/bin/foo` but not `bin/foobar`
      --at-root            Treat PATTERN as an absolute file path, so it only matches starting from the root of a package. This means that the pattern `/bin/foo` only matches a file called `/bin/foo` or `/bin/foobar` but not `/libexec/bin/foo`
      --minimal            Only print attribute names of found files or directories. Other details such as size or store path are omitted. This is useful for scripts that use the output of nix-locate
  -h, --help               Print help
  -V, --version            Print version


How to use
==========

In the simplest case, just run `nix-locate part/of/file/path` to search for all packages that contain
a file matching that path:

$ nix-locate 'bin/firefox'
...all packages containing a file named 'bin/firefox'

Before using this tool, you first need to generate a nix-index database.
Use the `nix-index` tool to do that.

Limitations
===========

* this tool can only find packages which are built by hydra, because only those packages
  will have file listings that are indexed by nix-index

* we can't know the precise attribute path for every package, so if you see the syntax `(attr)`
  in the output, that means that `attr` is not the target package but that it
  depends (perhaps indirectly) on the package that contains the searched file. Example:

  $ nix-locate 'bin/xmonad'
  (xmonad-with-packages.out)      0 s /nix/store/nl581g5kv3m2xnmmfgb678n91d7ll4vv-ghc-8.0.2-with-packages/bin/xmonad

  This means that we don't know what nixpkgs attribute produces /nix/store/nl581g5kv3m2xnmmfgb678n91d7ll4vv-ghc-8.0.2-with-packages,
  but we know that `xmonad-with-packages.out` requires it.
```

Key defaults:
- DB directory: `~/.cache/nix-index/` (`-d`, `--db`, env `NIX_INDEX_DATABASE`)
- `--type` defaults to all file types (`r`, `x`, `d`, `s`) when not given
- `--color` defaults to `auto`
- `--group` is enabled by default (`no-group` disables it)
- `pattern` is escaped as a literal unless `--regex` is given
- `--whole-name` and `--at-root` anchor the generated regex with `$`/`^` respectively

## `command-not-found` integration

### Bash (`command-not-found.sh`, line 19)

```sh
attrs=$(@out@/bin/nix-locate --minimal --no-group --type x --type s --whole-name --at-root "/bin/$cmd")
```

The shell hook defines `command_not_found_handle` (and `command_not_found_handler` for zsh). It:
- Uses `nixpkgs` as the default `toplevel` attribute.
- If `NIX_AUTO_INSTALL` is set, runs `nix profile install nixpkgs#$attrs` or `nix-env -iA nixpkgs.$attrs`.
- If `NIX_AUTO_RUN` is set, runs `nix-build --no-out-link -A $attrs "<nixpkgs>"` then `nix-shell -p $attrs --run ...`.
- Otherwise it prints `nix profile install nixpkgs#$attr` / `nix-env -iA nixpkgs.$attr` or `nix shell nixpkgs#$attr -c $cmd ...` / `nix-shell -p $attr --run ...`.

### Nushell (`command-not-found.nu`, line 32)

```nu
let pkgs = (@out@/bin/nix-locate --minimal --no-group --type x --type s --whole-name --at-root $"/bin/($cmd_name)" | lines)
```

It returns a message (or `null` if no package) with `nix shell nixpkgs#$pkg` suggestions.

## Database format

The database file is named `files` inside the `--db` directory.

### Header

- Magic: 4 bytes `NIXI` (`src/database.rs`, line 30)
- Format version: `u64` little-endian. Current constant is `1` (`src/database.rs`, line 26).

### Body

After the header, the rest is a single zstd stream (`src/database.rs`, lines 52-61 and 154-157), compressed with the configured level (`-c`, default `22`) and multi-threaded `num_cpus` workers.

The zstd stream contains a sequence of `frcode` blocks (`src/frcode.rs`, lines 1-54). Each block is a newline-separated sequence of entries.

### Entry encoding

Every entry is `metadata\0<shared-prefix-differential><path>\n` (`src/frcode.rs`, lines 9-53):

- `metadata` is a `NUL`-terminated byte blob.
- The `<shared-prefix-differential>` is a variable-length signed `i16`: one byte if the absolute difference is within `i8::MAX`, otherwise `0x80` followed by two bytes (big-endian). The differential is added to the previous shared prefix length to get the current shared prefix length.
- The remaining path bytes after the differential complete the file path.
- The entry is terminated by `\n`.

A file entry's metadata is:
- `SIZE` + `x` or `r` for regular files (executable vs non-executable) (`src/files.rs`, lines 144-147 and 162-167)
- `TARGET` + `s` for symlinks (`src/files.rs`, lines 148-150 and 169-171)
- `SIZE` + `d` for directories (`src/files.rs`, lines 152-153 and 172-175)

Each package group is followed by a package entry:

- `p\0<json>\n` (`src/database.rs`, lines 79-83)
- The JSON is a serialized `StorePath` (`src/package.rs`, lines 134-139), with fields `store_dir`, `hash`, `name`, and `origin` (`attr`, `output`, `toplevel`, `system`).

### `paths.cache`

When `--path-cache` is used, `nix-index` writes `paths.cache` in the current working directory (`src/bin/nix-index.rs`, lines 105-116). It is a `bincode` 2.x encoded `Vec<(StorePath, String, FileTree)>` (`src/listings.rs`, lines 102-125 and `src/listings.rs`, lines 98-112). It is intended only for development and is not verified for staleness.

### Example decoded small database

A dump of the generated small database (using `examples/nix-index-debug.rs`) shows entries like:

```
"93728x\0/bin/xargs"
"271496x\0/bin/find"
"p\0{\"store_dir\":\"/nix/store\",\"hash\":\"f4bixss3h2i80asiz45aj4qmplvmam4k\",\"name\":\"findutils-4.10.0\",\"origin\":{\"attr\":\"findutils\",\"output\":\"out\",\"toplevel\":true,\"system\":\"x86_64-linux\"}}"
"coreutilss\0/bin/yes"
"coreutilss\0/bin/whoami"
...
```

This confirms the `metadata\0path` layout and the trailing `p\0<JSON>` package record.

## Observed performance

### Build of `nix-index` itself

- `cargo clean && cargo build` (debug): real `0m6.609s`, user `0m34.978s`, sys `0m9.169s`
- `cargo build --release` (optimized): `Finished release profile in 40.45s`
- Debug binary sizes: `nix-index` 34M, `nix-locate` 18M
- Release binary sizes: `nix-index` 147M, `nix-locate` 138M (large because `Cargo.toml` sets `debug = true` in the release profile)

### Database build

A small nixpkgs set in `research/small.nix`:

```nix
with import <nixpkgs> {};
{
  coreutils = coreutils;
  bash = bash;
  hello = hello;
  findutils = findutils;
  gnutar = gnutar;
  gzip = gzip;
  firefox = firefox;
}
```

Built with the release binary:

```
nix-index --filter-prefix /bin/ -f small.nix --extra-scopes "" --db nix-index-db-small
```

Result:
- real build time: `0m1.713s`
- database size: `10,756` bytes (`nix-index-db-small/files`)
- The generated `files` contains `NIXI\x01\0...` followed by zstd data.

A tiny `hello`-only database (`tiny.nix`) was `173` bytes and built in `~0.5s`.

### `nix-locate` query times

Using the release `nix-locate` against the `small.nix` database:

```
$ nix-locate --db nix-index-db-small bin/ls
  coreutils.out                                         0 s /nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-coreutils-9.11/bin/ls

$ nix-locate --db nix-index-db-small bin/firefox
  firefox.out                                      16,524 x /nix/store/lx2iz74c0rkvzqyaa8fzjaw2szqc9c8s-firefox-152.0.5/bin/firefox
```

`hyperfine --shell=none --warmup 5 -r 20`:

- `bin/ls`: `950.2 µs ± 152.6 µs`
- `bin/firefox`: `1.0 ms ± 0.2 ms`

These numbers are for a 10 KB database; startup dominates. A full nixpkgs database would be much larger and I/O-bound during the zstd/frcode decode.

## nixdex-specific extensions

This section documents flags and behaviours that nixdex adds over upstream `nix-index`.

### `nixdex index` extensions

- `--select <EXPR>` — passed through to `nix-eval-jobs --select`. Must be a Nix function accepting the package set, for example `p: { inherit (p) hello coreutils; }`.
- `--exclude-prefix <PREFIX>` — skip paths starting with prefix; may be given multiple times.
- `--small` — build a small database containing only `/bin/` entries (equivalent to `--filter-prefix /bin/`).
- `--no-check-cache-status` — disable `nix-eval-jobs --check-cache-status` to avoid blocking eval workers on narinfo lookups.
- `--no-main-program` — do not synthesize `/bin/<mainProgram>` listings from `meta.mainProgram`.
- `--download-prebuilt` — download a prebuilt `files` database from the `nix-index-database` release assets instead of evaluating nixpkgs locally.
- `--prebuilt-url`, `--prebuilt-arch`, `--prebuilt-small` — control prebuilt download source and variant.
- `--path-cache`, `--path-cache-file`, `--cache-key`, `--path-cache-ttl`, `--force` — manage the `paths.cache` development cache.
- `--only-eval` — only evaluate nixpkgs; do not fetch listings or write the `files` database.
- `--format-version 1|2` — on-disk database format. Default is `2`, a nixdex extension; `1` is fully upstream-compatible.

### `nix-locate` extensions

- `--json` — output one JSON object per line.
- `--sort`, `--min-size`, `--max-size`, `--exclude-fhs`, `--limit` — additional output controls.
- `--color always|never|auto` — colour output policy.
- `--all` and `--minimal` behave as in upstream, but `(attr)` output is sorted so top-level packages are preferred when no `--all` is given.

### Environment variables

- `NIX_INDEX_DATABASE` — directory used by both upstream and nixdex for the `files` index and sidecars.
- `NIXDEX_DATABASE` — optional override used by `command-not-found` integration to point at a dedicated, often `/bin/`-filtered database, while leaving `NIX_INDEX_DATABASE` for general `nix-locate` queries. The shell integration scripts (`command-not-found.sh`, `command-not-found.fish`, `command-not-found.nu`) prefer `NIXDEX_DATABASE` and fall back to `NIX_INDEX_DATABASE`.

### Database format compatibility

nixdex can read both v1 (upstream) and v2 (nixdex extension) `files` databases. `nixdex index` writes v2 by default; use `--format-version 1` to produce a database readable by upstream `nix-index`.

## Blockers / notes

1. **System `sqlite3` missing.** A plain `cargo build` in `.upstream/nix-index` fails to link because `rusqlite` needs a system `sqlite3`. This was fixed by building inside `nix-shell -p sqlite pkg-config`.
2. **Prebuilt nixpkgs binary is older.** `nix run nixpkgs#nix-index -- --version` reports `0.1.10`, and it returned `ParseResponse` errors on zstd-compressed `.ls` files (e.g. `https://cache.nixos.org/pg2zfrrbm58ynbjshhzkgg4q466spinf.ls`). The local `0.1.12` build parsed the same files correctly.
3. **Full nixpkgs index not generated.** `nix-index --filter-prefix /bin/` against `<nixpkgs>` started with ~168k paths in the queue and would have taken an impractical amount of time. All timing is therefore on small, deliberately-scoped databases.
4. **Release binaries are huge** (`nix-index` 147M, `nix-locate` 138M) because the upstream `Cargo.toml` sets `debug = true` in the release profile (`Cargo.toml`, line 70).
5. **Default `--extra-scopes`** includes `haskellPackages`, `rPackages`, `coqPackages`, `texlive.pkgs`. When using a small test `nixpkgs` file, pass `--extra-scopes ""` to avoid `nix-env` attribute errors.

## Files generated for this report

All outputs are in `/home/w0w/dev/nixdex/research/`:
- `baseline.md` (this report)
- `tiny.nix` (single `hello` test set)
- `small.nix` (small set used for timing and DB size)
- `nix-index-db/` (`hello`-only database, 173 bytes)
- `nix-index-db-small/` (database for `small.nix`, 10.8 KB)

The upstream build artifacts are in `/home/w0w/dev/nixdex/.upstream/nix-index/target/` only.
