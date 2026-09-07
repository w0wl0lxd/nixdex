- Apply `--limit` after the sort for short literal patterns. A pattern under
  three characters routes to `search_short_literal`, which stopped collecting
  at the limit, so `--sort size --limit 1` returned whichever match the index
  reached first rather than the smallest.
- Drop a basename-FST lookup whose result was discarded. It ran on every short
  literal query and could not become an early return: the search matches a
  substring of the full path, so a basename miss does not rule out matches.
- Warn when a database has every sidecar but no command index. Indexing writes
  it, not `generate-sidecars`, so the skip path reported "up to date" and the
  daemon then rejected the reload with nothing explaining why.
