# nix-eval-jobs + binary cache API verification

Date: 2026-07-15

## 1. Tools

- `nix` 2.35.1 — available at `/run/current-system/sw/bin/nix`
- `nix-eval-jobs` — not on the default `PATH`; installed/executed via `nix run nixpkgs#nix-eval-jobs` (resolved to `nix-eval-jobs 2.34.3`)

## 2. nix-eval-jobs

### Command that worked

`nix-eval-jobs` emits JSON Lines (NDJSON) by default; the `--json` flag is **not** supported in this version.

```bash
nix run nixpkgs#nix-eval-jobs -- \
  --expr 'import <nixpkgs> { config = { allowAliases = false; }; }' \
  --select 'pkgs: { inherit (pkgs) hello; }' \
  --check-cache-status
```

To capture a small sample quickly:

```bash
timeout 10s nix run nixpkgs#nix-eval-jobs -- \
  --expr 'import <nixpkgs> { config = { allowAliases = false; }; }' \
  --select 'pkgs: { inherit (pkgs) hello; }' \
  --check-cache-status 2>/dev/null
```

### Sample output line

Saved at `/home/w0w/dev/nixdex/research/eval-sample.json`:

```json
{"attr":"hello","attrPath":["hello"],"cacheStatus":"local","constituents":[],"drvPath":"/nix/store/vvjw1pyn64q08yfil2z8mdaqdhz7skqh-hello-2.12.3.drv","globConstituents":false,"isCached":true,"name":"hello-2.12.3","namedConstituents":[],"neededBuilds":[],"neededSubstitutes":[],"outputs":{"out":"/nix/store/pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3"},"requiredSystemFeatures":[],"storeDir":"/nix/store","system":"x86_64-linux"}
```

### Field meanings

| Field | Meaning |
|-------|---------|
| `attr` | Short attribute name (last component of `attrPath`). |
| `attrPath` | List of attribute names from the root to this derivation. |
| `drvPath` | Path to the `.drv` file in the Nix store. |
| `outputs` | Object mapping output names (`out`, `dev`, `man`, etc.) to store paths. |
| `name` | Derivation name, e.g. `coreaction-5.0.1`. |
| `system` | Target platform, e.g. `x86_64-linux`. |
| `cacheStatus` | Cache availability; present only with `--check-cache-status`. Observed values: `cached` (available in a configured substituter), `local` (already in the local store). |
| `isCached` | Boolean indicating whether the output is in the cache. |
| `neededBuilds` | Store paths that must be built. |
| `neededSubstitutes` | Store paths that can be fetched from a substituter. |
| `constituents`, `namedConstituents`, `globConstituents` | Hydra aggregate / constituent metadata. |
| `requiredSystemFeatures` | Features the builder must support (e.g. `kvm`, `big-parallel`). |
| `storeDir` | Nix store directory, typically `/nix/store`. |

Error lines are also JSON objects containing `error` (string) and `fatal` (boolean). For example, `AAAAAASomeThingsFailToEvaluate` and unfree/broken packages produce such lines.

### Blockers / notes

- Evaluating the entire `nixpkgs` set produces errors from `AAAAAASomeThingsFailToEvaluate`, unfree packages, broken packages, etc. A real consumer of `nix-eval-jobs` must filter `fatal`/`error` lines, allow `allowUnfree`, and handle `check-meta` failures.
- The `--select` option can be used to limit the evaluated tree (e.g. `--select 'pkgs: { inherit (pkgs) hello; }'`), which avoids the full-tree noise.
- `--check-cache-status` is required to add `cacheStatus` / `isCached`; without it, those fields are absent.

## 3. Binary cache `.narinfo` API

### Example

For `nixpkgs#hello`:

```bash
$ nix build nixpkgs#hello --no-link --print-out-paths
/nix/store/pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3

$ HASH=pg2zfrrbm58ynbjshhzkgg4q466spinf
$ curl -s "https://cache.nixos.org/${HASH}.narinfo" | head -20
```

Response:

```text
StorePath: /nix/store/pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3
URL: nar/14qxzyn4mjn5gqyfwdq0rvr83q1hfy7z0gzbqhyds62kh7q2m46c.nar.zst
Compression: zstd
FileHash: sha256:1iphb940lyf0pvhd23irrwnpf065y8qvplnpcpw207zl1a7cdc6p
FileSize: 75355
NarHash: sha256:14qxzyn4mjn5gqyfwdq0rvr83q1hfy7z0gzbqhyds62kh7q2m46c
NarSize: 279624
References: ias8xacs1h3jy7xgwi2awvim61k2ji6c-glibc-2.42-67 pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3
Deriver: m10br6npilinjz3ly2hz8x3clb9lidx9-hello-2.12.3.drv
Sig: cache.nixos.org-1:ngMSyeL2+RJMgNKgd84M+rJegrC4w9kWOJLMr916YxYmwAfDKdozkLe4QgIP0T9+FtEaCf/PhBJbfE/KOzLxAQ==
```

### Field reference

| Field | Meaning |
|-------|---------|
| `StorePath` | Full store path for this output. The hash part is the `.narinfo` request key. |
| `URL` | Relative URL to the compressed NAR archive. Concatenate to `https://cache.nixos.org/` to download. Form: `nar/<hash>.nar[.<ext>]`. |
| `Compression` | Compression algorithm used for the archive in `URL`, e.g. `zstd`, `xz`, or empty (uncompressed). |
| `FileHash` | SHA-256 of the compressed file (`SRI` base-32 format). |
| `FileSize` | Compressed file size in bytes. |
| `NarHash` | SHA-256 of the uncompressed NAR. |
| `NarSize` | Uncompressed NAR size in bytes. |
| `References` | Space-separated list of store paths this output depends on at runtime. Each entry is a basename (`<hash>-<name>`); prepend `/nix/store/` to reconstruct full paths. |
| `Deriver` | Basename of the `.drv` that built this output. |
| `Sig` | Signature of the `cache.nixos.org` substituter, in the form `<key>:<base64>`. |

### Parsing notes

- `StorePath` can be split as `/nix/store/<hash>-<name>`. The `<hash>` is the base32 store hash and is the key used for `https://cache.nixos.org/<hash>.narinfo`.
- `URL` is relative; the full archive URL is `https://cache.nixos.org/<URL>`.
- `References` is a single space-separated line; split on whitespace and map each entry `X` to `/nix/store/X`.
- `FileHash` and `NarHash` use Nix's SRI base-32 representation.

## 4. Binary cache `.ls` API

### Endpoint

`https://cache.nixos.org/<hash>.ls` returns a JSON listing of the store path contents.

```bash
$ HASH=pg2zfrrbm58ynbjshhzkgg4q466spinf
$ curl -s -o /tmp/hello.ls "https://cache.nixos.org/${HASH}.ls"
```

### Compression detection

`cache.nixos.org` sends the `.ls` payload with `content-encoding: zstd` and the raw bytes are zstd compressed. Check the first bytes:

| Magic bytes | Format |
|-------------|--------|
| `0x28 0xb5 0x2f 0xfd` | zstd |
| `0xfd 0x37 0x7a 0x58 0x5a 0x00` | xz |
| `0x7b` (`{`) or starts with whitespace then `{` | plain JSON |

For `hello`:

```bash
$ od -An -tx1 -N 8 /tmp/hello.ls
 28 b5 2f fd 00 58 ed 10
```

First four bytes `28 b5 2f fd` confirm zstd.

### Decompression

```bash
$ zstd -d -c /tmp/hello.ls | head -c 2000
{"root":{"entries":{"bin":{...}}}}
```

### Sample JSON

Saved at `/home/w0w/dev/nixdex/research/sample-ls.json` (extract of the `bin` directory):

```json
{
  "entries": {
    "hello": {
      "executable": true,
      "size": 64472,
      "type": "regular"
    }
  },
  "type": "directory"
}
```

### Schema

The decompressed `.ls` JSON is a single object with a top-level `root` key:

```json
{
  "root": {
    "type": "directory",
    "entries": {
      "<name>": { <node> },
      ...
    }
  }
}
```

Each node object has a `type` field and additional fields based on the type:

| `type` | Additional fields | Meaning |
|--------|-------------------|---------|
| `regular` | `size` (int), `executable` (bool) | Normal file. |
| `directory` | `entries` (object) | Directory containing more nodes keyed by name. |
| `symlink` | `target` (string) | Symbolic link pointing to `target`. |

### Example symlink

From `bash` (path `bbzjxfam8vv1nyikn5dsrazsw4ya5vzx`):

```json
{
  "target": "bash",
  "type": "symlink"
}
```

### Blockers / notes

- The `.ls` endpoint is **not always available** for every store path; some caches or paths may return 404. `nix-eval-jobs` with `--check-cache-status` is the right way to discover whether a path is available for substitution before trying `.ls`.
- Content is compressed with zstd by `cache.nixos.org`, but other caches may use xz or serve plain JSON. Always sniff the magic bytes and honor the `Content-Encoding`/`content-type` headers.
- The `.ls` tree can be large; stream/decompress and parse incrementally when enumerating big packages.
