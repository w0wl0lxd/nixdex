# Contributing

## Commit messages (Conventional Commits)

Every commit subject **must** match:

```text
<type>[optional scope][optional !]: <description>
```

| Type | Use for |
|------|---------|
| `feat` | New user-facing capability |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `style` | Formatting / whitespace (no logic change) |
| `refactor` | Internal change that is not a fix or feat |
| `perf` | Performance improvement |
| `test` | Adding or fixing tests |
| `build` | Build system / deps (Cargo, Nix, tooling) |
| `ci` | CI configuration / workflows |
| `chore` | Maintenance that does not fit above |
| `revert` | Reverts a previous commit |

Rules:

- Subject ≤ 72 characters; prefer ≤ 50.
- Imperative mood: `add`, `fix`, `remove` — not `Added` / `Fixes`.
- No trailing period on the subject.
- Blank line between subject and body when a body is present.
- Body explains **why**, not a restatement of the diff.

Good:

```text
feat(cli): support --template for truss new

Default remains "default"; registry entries override embedded packs.
```

Bad:

```text
truss: implement core new/sync/check
Added template rendering.
feat: wip
```

## No AI attribution (hard rule)

Never put any of the following in commits, PR titles, PR bodies, changelogs, or work-logs:

- `Generated with [Devin]` / Claude / Cursor / Copilot / Codex / …
- `Co-Authored-By:` trailers for agent or bot accounts
- `Assisted-by:` / `Generated-by:` AI footers
- Robot emoji attribution lines
- Agent emails (`agent@devin`, `noreply@anthropic.com`, `devin-ai-integration[bot]`, …)

Authorship is the human Git author (`user.name` / `user.email`). Hooks enforce this
locally; CI enforces it on PRs.

## PR titles and descriptions (no slop)

**Title** — same Conventional Commits shape as a commit subject, ≤ 72 chars.

**Body** — use `.github/PULL_REQUEST_TEMPLATE.md`. Required:

1. **Summary** — 1–3 concrete bullets of *what changed and why*.
2. **Type** — checkbox.
3. **Test plan** — commands actually run (or “N/A” with reason).
4. **Notes** — risks, follow-ups, screenshots only when useful.

Rejects (CI / review):

- Titles like `Update`, `Fixes`, `WIP`, `stuff`, `misc`, `changes`
- Bodies that are only “Generated with …”, empty template sections, or lorem
- AI watermark sections

## Local git hooks

Hooks live in `.githooks/` (tracked). Activate once per clone:

```bash
just setup-hooks
# equivalent: git config core.hooksPath .githooks
```

| Hook | Role |
|------|------|
| `prepare-commit-msg` | Soft-strips known AI trailers before the editor |
| `commit-msg` | Hard-fails on AI attribution, non-conventional subjects, placeholder slop |
| `pre-push` | Scans commits being pushed for AI trailers, agent authors, non-conventional subjects |

Bypass (emergency only): `git commit --no-verify` / `git push --no-verify`.

## AI agent files

Root `AGENTS.md`, `CLAUDE.md`, and tool dirs (`.claude/`, `.cursor/`, `.devin/`, …)
are **gitignored**. Keep personal agent loaders local. Product conventions for
humans and reviewers live in this file and `README.md`.

Scaffold templates may still embed `AGENTS.md` / `CLAUDE.md` **inside** template
trees (e.g. `crates/truss-cli/templates/default/`) — those are product output,
not repo agent config.

## Verification before opening a PR

```bash
just setup-hooks   # once
just validate      # fmt + check + clippy + test
```
