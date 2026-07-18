# nixdex

Modern Rust rewrite of [`nix-index`](https://github.com/nix-community/nix-index):
fast package file indexing and `nix-locate`-compatible search.

`nix-locate` output and the v1 database format are fully upstream-compatible.
`nixdex index` defaults to database format v2, a nixdex extension; use
`--format-version 1` to produce a database readable by upstream `nix-index`.

## Workspace

| Crate | Role |
|-------|------|
| `nixdex-core` | Store paths, listings, index, database |
| `nixdex-cli` | `nix-index` and `nix-locate` binaries |
| `nixdex-daemon` | Optional background indexer |

## Quick start

```bash
just setup-hooks   # once per clone
cargo run --bin nix-locate -- --help
cargo run --bin nix-index -- --help
```

## Development

```bash
just validate      # fmt + check + clippy + nextest
```

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for Conventional Commits, no-AI-attribution
policy, PR hygiene, and local hooks.
