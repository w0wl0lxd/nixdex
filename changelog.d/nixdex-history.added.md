Added `nixdex history <attr>` subcommand for querying package version history.

The `nixdex-history` crate provides a `files.history` sidecar that maps
attribute paths to lists of `(version, commit, date)` entries. The
`nixdex history` subcommand shows when versions of a package existed.
