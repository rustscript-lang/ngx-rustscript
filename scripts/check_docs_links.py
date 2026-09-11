#!/usr/bin/env python3
"""Verify relative Markdown links under README.md and docs/."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOC_ROOTS = [ROOT / "README.md", ROOT / "docs"]

LINK_RE = re.compile(r"(?<!!)\[([^\]]*)\]\(([^)]+)\)")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$")


def github_slug(text: str) -> str:
    text = re.sub(r"<[^>]+>", "", text)
    text = text.strip().lower()
    text = re.sub(r"[^\w\s-]", "", text, flags=re.UNICODE)
    text = re.sub(r"\s+", "-", text)
    return text


def markdown_files() -> list[Path]:
    files = [ROOT / "README.md"]
    files.extend(sorted((ROOT / "docs").rglob("*.md")))
    return files


def headings(path: Path) -> set[str]:
    slugs: set[str] = set()
    seen: dict[str, int] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        match = HEADING_RE.match(line)
        if not match:
            continue
        slug = github_slug(match.group(2))
        count = seen.get(slug, 0)
        seen[slug] = count + 1
        slugs.add(slug if count == 0 else f"{slug}-{count}")
    return slugs


def split_target(target: str) -> tuple[str, str | None]:
    if target.startswith("#"):
        return "", target[1:]
    if "#" in target:
        path, anchor = target.split("#", 1)
        return path, anchor
    return target, None


def main() -> int:
    files = markdown_files()
    heading_cache = {path.resolve(): headings(path) for path in files}
    errors: list[str] = []
    checked = 0

    for path in files:
        text = path.read_text(encoding="utf-8")
        for match in LINK_RE.finditer(text):
            raw = match.group(2).strip()
            if raw.startswith("<") and raw.endswith(">"):
                raw = raw[1:-1]
            if re.match(r"^[a-z][a-z0-9+.-]*:", raw, re.I):
                continue
            if raw.startswith("//"):
                continue
            rel, anchor = split_target(raw)
            checked += 1
            if rel:
                dest = (path.parent / rel).resolve()
            else:
                dest = path.resolve()
            if not dest.exists():
                errors.append(f"{path.relative_to(ROOT)}: missing {raw}")
                continue
            if dest.is_dir():
                index = dest / "index.md"
                if not index.exists():
                    errors.append(
                        f"{path.relative_to(ROOT)}: directory has no index.md: {raw}"
                    )
                dest = index if index.exists() else dest
            if anchor is None:
                continue
            if dest.suffix != ".md":
                continue
            slugs = heading_cache.get(dest)
            if slugs is None:
                slugs = headings(dest)
                heading_cache[dest] = slugs
            if github_slug(anchor) not in slugs and anchor not in slugs:
                errors.append(
                    f"{path.relative_to(ROOT)}: missing heading #{anchor} in "
                    f"{dest.relative_to(ROOT)}"
                )

    print(f"checked {checked} relative links in {len(files)} markdown files")
    if errors:
        print(f"{len(errors)} error(s):")
        for err in errors:
            print(f"  {err}")
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
