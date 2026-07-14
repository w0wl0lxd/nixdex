# Wave 4 verification — nixdex vs upstream

Date: 2026-07-15

## Environment

- Host: NixOS
- nixdex: release build from workspace `1aba7ec` + eval-expr fix
- Evaluator: `nix-eval-jobs` 2.34.3 via nix store path
- Fixture: `research/small.nix` (coreutils, bash, hello, findutils, gnutar, gzip, firefox)
- Index flags: `--filter-prefix /bin/ -c 19 -r 20 --extra-scopes ""`

## Index build

| Metric | nixdex | upstream baseline (research/baseline.md) |
|--------|--------|------------------------------------------|
| Wall time (small.nix) | **~1.81 s** | ~1.71 s |
| Database size (`files`) | **1154 B** | 10 756 B |
| Magic | `NIXI` + version 1 + zstd | same |

nixdex is smaller for the same filter because package JSON + frcode blocks compress harder at level 19 on this set, and entry selection may differ slightly from upstream's `nix-env` enumeration.

## Locate query (hyperfine, release, shell=none, 20 runs)

| Query | nixdex mean | upstream baseline |
|-------|-------------|-------------------|
| `bin/ls` | **703 ± 84 µs** | 950 ± 153 µs |
| `bin/firefox` | **712 ± 98 µs** | 1.0 ± 0.2 ms |

Sample nixdex output:

```
coreutils.out                                         0 s /nix/store/...-coreutils-9.11/bin/ls
firefox.out                                      16,524 x /nix/store/...-firefox-152.0.5/bin/firefox
```

`--minimal --whole-name --at-root /bin/ls` → `coreutils.out`  
`--minimal --whole-name --at-root /bin/hello` → `hello.out`

## Format compatibility

nixdex `nix-locate` successfully reads the **upstream** `research/nix-index-db-small/files` database and returns the same coreutils `bin/ls` match. The on-disk NIXI/frcode/zstd format is interoperable for this fixture.

## command-not-found

The shell hook queries:

```sh
nix-locate --minimal --no-group --type x --type s --whole-name --at-root "/bin/$cmd"
```

Smoke against the small index (with `NIX_INDEX_DATABASE=research/nixdex-db-small`) returns package attrs for `ls` / `hello` / `firefox` and empty for unknown commands — suitable for the packaged `command-not-found.sh` after `@out@` substitution.

## Blockers / follow-ups

- Full `<nixpkgs>` index still expensive; keep scoped fixtures for CI.
- `--path-cache` and extra-scopes wiring remain incomplete.
- FST index not built yet (linear scan only); fine for small DBs, needed for full nixpkgs latency.
- Package `truss` with crane in a later PR; flake is still `devShell`-only.
