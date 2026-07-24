Added `--stream` and `--format` flags to `nixdex search` and `nixdex locate`.

- `--stream` flushes output after each result for better piping behavior.
- `--format` selects output format: `table` (default), `ndjson`, or `csv`.
