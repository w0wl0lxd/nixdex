# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `nixdex info` text output now includes `main_program` for each output.
- `nixdex search` supports `--sort` by `attr`, `name`, and `main-program` in ascending or descending order.
- `nixdex index` accepts `--no-overlays` to disable nixpkgs overlays during evaluation.
- `nixdex index` accepts `--no-closure` to skip runtime reference traversal when fetching `.ls` listings.
- `nixdex index` accepts `--timeout` and `--retries` to configure binary-cache HTTP requests.
- `nixdex command-not-found` suggests running a missing command with `comma` when `,` is on `$PATH`.
- `nixdex command-not-found` supports `--interactive` and `NIX_AUTO_RUN_INTERACTIVE` for provider selection before auto-run.
- `nixdex-daemon` exposes `/version` and `/ready` HTTP endpoints.
