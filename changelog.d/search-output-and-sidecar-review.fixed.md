`--stream` now flushes after every result for every output format. It previously flushed only in the plain table branch, so `--format ndjson|csv|yaml` and `--json` stayed buffered when piped. `--json` is normalised to `--format ndjson` instead of carrying a duplicate serialization loop.

`--exclude` and `--exclude-regex` no longer consume the `--limit` budget. Exclusions run after the search, so a truncated result set dropped eligible matches beyond the limit and `--count` underreported them. The search is left unbounded when an exclusion is set and the limit is applied to the surviving rows.

Option descriptions lose their `| ` Markdown table prefix once, at ingest and when a sidecar is decoded, rather than only when a human-readable record is printed. `--json`, the daemon's `/options` response and `OptionsDb::search` all saw the prefixed text and matched against it.

`nixdex update` refuses a `--release-url` that is not HTTPS, except on loopback. A sidecar is installed after only a four-byte magic check and there is no signature or checksum, so transport security is the only integrity guarantee available. A failed sidecar download now also reports that the previous file was kept and may describe an older release, and removes the partial download instead of leaving `files.history.tmp` behind.

The missing-sidecar errors point at `nixdex update` rather than a `nix-index --options` flag that does not exist; local indexing does not produce these sidecars. The size caps are read from `nixdex_history::MAX_HISTORY_BYTES` and `nixdex_options::MAX_OPTIONS_BYTES` instead of being repeated as literals in the CLI.
