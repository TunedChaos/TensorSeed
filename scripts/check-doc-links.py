#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
"""Check that relative Markdown links resolve inside a directory tree.

Usage: check-doc-links.py <root> [--scan SUBDIR] [--allow-missing PREFIX ...]

Scans the Markdown under <root>/SUBDIR (default: all of <root>); links
must resolve to files inside <root>, except paths under an allowed
prefix.

`just dist` runs this on each staged release archive so a guide never
ships with a link to a file that was left out. Links to web pages,
anchors in the same file, and mail addresses are not checked; a link's
own `#anchor` part is ignored. Exits 1 and lists the broken links.
"""

import pathlib
import re
import sys

LINK = re.compile(r"\]\(([^)\s]+)\)")


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    root = pathlib.Path(argv[0]).resolve()
    allow = []
    scan = root
    rest = argv[1:]
    while rest:
        if rest[0] == "--allow-missing" and len(rest) > 1:
            allow.append(rest[1])
            rest = rest[2:]
        elif rest[0] == "--scan" and len(rest) > 1:
            scan = root / rest[1]
            rest = rest[2:]
        else:
            print(f"unknown argument {rest[0]}", file=sys.stderr)
            return 2
    bad = []
    for md in sorted(scan.rglob("*.md")):
        text = md.read_text(encoding="utf-8")
        # Fenced code blocks hold examples, not links.
        text = re.sub(r"```.*?```", "", text, flags=re.S)
        for target in LINK.findall(text):
            if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
                continue
            path = target.split("#", 1)[0]
            if not path:
                continue
            dest = (md.parent / path).resolve()
            rel = dest.relative_to(root) if dest.is_relative_to(root) else None
            if rel is not None and any(str(rel).startswith(a) for a in allow):
                continue
            if rel is None or not dest.exists():
                bad.append(f"{md.relative_to(root)}: {target}")
    for b in bad:
        print(f"broken link: {b}", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
