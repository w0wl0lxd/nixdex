Extended `nixdex-daemon` with new HTTP API endpoints: `/info`, `/history`, `/options`, and `/stats`.

- `GET /info?attr=<attr>` returns package metadata for a single attribute.
- `GET /history?attr=<attr>` returns version history for a package.
- `GET /options?pattern=<pattern>` searches NixOS module options.
- `GET /stats` returns database statistics including sidecar status.
