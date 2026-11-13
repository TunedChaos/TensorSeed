#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
set -e
if command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now tensorseed-serve >/dev/null 2>&1 || true
    systemctl daemon-reload >/dev/null 2>&1 || true
fi
