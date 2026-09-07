`nixdex locate --details` against the daemon no longer corrupts records: the
delimiter is now appended after the detail fields instead of before them, so a
record never splits across the delimiter boundary.

`/stats` reports `package_count` instead of the misleading `sidecar_entry_count`,
which never counted sidecars.

The history sidecar checks its per-attribute version cap only when it actually
adds a version, so re-recording a version an attribute already has no longer
fails once that attribute is full. The sidecar is also written to a temporary
file and renamed into place, so a failed write leaves the previous sidecar
intact.

Case-insensitive option search no longer allocates a lowercased copy of every
attribute and description for every query.

The TUI Nord theme uses the real Nord palette instead of the Tokyo Night one,
Tokyo Night uses its own palette, and `Theme::default()` now agrees with the
theme `App::new` starts with. The header block gets the three rows its border
needs, so the query line is visible again. Expired search-cache entries are
dropped on each tick instead of accumulating for the life of the session.

Daemon-backed `nixdex locate --json` emits package metadata whether or not
`--details` is passed, matching the local `--json` path, so the JSON schema no
longer depends on whether a daemon served the query.

The options sidecar is written to a temporary file and renamed into place, like
the history sidecar.
