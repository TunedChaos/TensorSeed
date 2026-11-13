#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
"""Substitute ${TENSORSEED_*} in the nfpm config from the environment."""

import os
import pathlib
import sys

KEYS = ("TENSORSEED_VERSION", "TENSORSEED_ARCH", "TENSORSEED_BIN")


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <in.yaml> <out.yaml>")
    text = pathlib.Path(sys.argv[1]).read_text()
    for key in KEYS:
        if key not in os.environ:
            sys.exit(f"{key} is not set")
        text = text.replace("${" + key + "}", os.environ[key])
    pathlib.Path(sys.argv[2]).write_text(text)


if __name__ == "__main__":
    main()
