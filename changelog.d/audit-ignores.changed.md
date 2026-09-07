- Update `h2` to 0.4.19 and `chacha20` to 0.10.2, and record the three
  remaining `cargo audit` advisories in `.cargo/audit.toml`. They arrive
  through `ratatui`, which still depends on `paste` and pins `lru = "^0.12"`,
  so no reachable release resolves them.
