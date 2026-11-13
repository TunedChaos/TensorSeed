#!/usr/bin/env python3
"""Generate site/xch-donate.svg from site/xch-donate-address.txt.

Run from the repo root. Needs the segno package (pure Python, no network
at encode time). deploy.sh installs it into a local venv if needed.
"""

from __future__ import annotations

import sys
from pathlib import Path

SITE = Path(__file__).resolve().parent.parent / "site"
ADDRESS_FILE = SITE / "xch-donate-address.txt"
OUTPUT = SITE / "xch-donate.svg"


def main() -> int:
    try:
        import segno
    except ImportError:
        print("generate-xch-qr.py needs the segno package", file=sys.stderr)
        return 1

    address = ADDRESS_FILE.read_text(encoding="utf-8").strip()
    if not address.startswith("xch1") or "\n" in address or " " in address:
        print("refusing to encode an unexpected address in", ADDRESS_FILE, file=sys.stderr)
        return 1

    qr = segno.make(address, error="M", micro=False, boost_error=False)
    qr.save(
        OUTPUT,
        kind="svg",
        scale=8,
        border=4,
        dark="#10140c",
        light="#efe8d4",
        xmldecl=True,
        svgns=True,
        omitsize=False,
        title="Tensorseed XCH support address",
    )
    print(f"wrote {OUTPUT} (QR version {qr.version}, error {qr.error})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
