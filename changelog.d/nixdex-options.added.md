Added `nixdex options <pattern>` subcommand for searching NixOS module options.

The `nixdex-options` crate provides a `files.options` sidecar that stores
NixOS module option records (attribute path, type, description, default
value, example). The `nixdex options` subcommand searches module options
by pattern.
