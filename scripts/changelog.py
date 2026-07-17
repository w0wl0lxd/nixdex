#!/usr/bin/env python3
"""Manage changelog.d fragments and regenerate CHANGELOG.md.

Usage:
    scripts/changelog.py collect
    scripts/changelog.py check [--base REF]

Fragments live in changelog.d/ and are named:

    <identifier>.<section>.md

where <section> is one of:
    added, changed, deprecated, removed, fixed, security

Each fragment contains one or more Markdown bullet lines starting with "- ".
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
CHANGELOG = REPO_ROOT / "CHANGELOG.md"
FRAGMENTS_DIR = REPO_ROOT / "changelog.d"

SECTIONS = ("added", "changed", "deprecated", "removed", "fixed", "security")
SECTION_ORDER = {
    "added": 0,
    "changed": 1,
    "deprecated": 2,
    "removed": 3,
    "fixed": 4,
    "security": 5,
}
SECTION_HEADER = {
    "added": "### Added",
    "changed": "### Changed",
    "deprecated": "### Deprecated",
    "removed": "### Removed",
    "fixed": "### Fixed",
    "security": "### Security",
}

UNRELEASED_HEADER = "## [Unreleased]"


def _fragments() -> list[Path]:
    if not FRAGMENTS_DIR.exists():
        return []
    files = sorted(p for p in FRAGMENTS_DIR.iterdir() if p.suffix == ".md")
    return files


def _parse_fragment_name(path: Path) -> tuple[str, str] | None:
    """Return (section, identifier) for a valid fragment, or None."""
    stem = path.stem
    for section in SECTIONS:
        if stem.endswith(f".{section}"):
            identifier = stem[: -len(section) - 1]
            return section, identifier
    return None


def _read_bullets(path: Path) -> list[str]:
    lines: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.rstrip()
        if not stripped:
            continue
        if stripped.startswith("- "):
            lines.append(stripped)
        else:
            raise ValueError(f"{path}: lines must be bullets starting with '- '")
    return lines


def _collect_fragments() -> dict[str, list[str]]:
    section_bullets: dict[str, list[str]] = {s: [] for s in SECTIONS}
    for path in _fragments():
        parsed = _parse_fragment_name(path)
        if parsed is None:
            raise ValueError(
                f"{path.name}: invalid fragment name. "
                f"Expected '<identifier>.<section>.md' where section is one of {SECTIONS}"
            )
        section, _identifier = parsed
        section_bullets[section].extend(_read_bullets(path))
    return section_bullets


def _existing_released_sections(changelog: str) -> str:
    """Return the portion of CHANGELOG.md after the first released version header."""
    pattern = re.compile(r"^## \[\d", re.MULTILINE)
    match = pattern.search(changelog)
    if not match:
        return ""
    return changelog[match.start() :]


def _generate_unreleased(section_bullets: dict[str, list[str]]) -> str:
    lines = [UNRELEASED_HEADER, ""]
    active_sections = [s for s in SECTIONS if section_bullets[s]]
    if not active_sections:
        lines.append("_No unreleased changes yet._")
        lines.append("")
    else:
        for section in sorted(active_sections, key=lambda s: SECTION_ORDER[s]):
            lines.append(SECTION_HEADER[section])
            for bullet in section_bullets[section]:
                lines.append(bullet)
            lines.append("")
    return "\n".join(lines)


def collect(_args: argparse.Namespace | None = None) -> int:
    section_bullets = _collect_fragments()
    released = ""
    if CHANGELOG.exists():
        released = _existing_released_sections(CHANGELOG.read_text(encoding="utf-8"))

    header = "# Changelog\n\nAll notable changes to this project are documented in this file.\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),\nand this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).\n\n"
    unreleased = _generate_unreleased(section_bullets)
    if released:
        body = f"{header}{unreleased}{released}"
    else:
        body = f"{header}{unreleased}"

    CHANGELOG.write_text(body, encoding="utf-8")
    print(f"Regenerated {CHANGELOG.relative_to(REPO_ROOT)}")
    return 0


def _run_git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=REPO_ROOT, text=True).strip()


def check(args: argparse.Namespace) -> int:
    base: str | None = args.base
    errors: list[str] = []

    for path in _fragments():
        parsed = _parse_fragment_name(path)
        if parsed is None:
            errors.append(
                f"{path.name}: invalid name (expected '<identifier>.<section>.md')"
            )
            continue
        try:
            _read_bullets(path)
        except ValueError as exc:
            errors.append(str(exc))

    if errors:
        print("Invalid changelog fragments:", file=sys.stderr)
        for err in errors:
            print(f"  {err}", file=sys.stderr)
        return 1

    if base is None:
        return 0

    try:
        diff_files = _run_git("diff", "--name-only", "--diff-filter=A", f"{base}...HEAD")
    except subprocess.CalledProcessError:
        diff_files = _run_git("diff", "--name-only", "--diff-filter=A", base)

    added_fragments = [f for f in diff_files.splitlines() if f.startswith("changelog.d/")]
    if not added_fragments:
        print(
            f"No changelog.d fragment added since {base}. "
            "Add one under changelog.d/ unless this PR is exempt.",
            file=sys.stderr,
        )
        return 1

    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Manage changelog.d fragments")
    sub = parser.add_subparsers(dest="command", required=True)

    collect_cmd = sub.add_parser("collect", help="Regenerate CHANGELOG.md from fragments")
    collect_cmd.set_defaults(func=collect)

    check_cmd = sub.add_parser("check", help="Validate fragments and that one was added on this branch")
    check_cmd.add_argument("--base", default="origin/main", help="Base ref to compare against")
    check_cmd.set_defaults(func=check)

    args = parser.parse_args()
    return args.func(args)  # type: ignore[no-any-return]


if __name__ == "__main__":
    sys.exit(main())
