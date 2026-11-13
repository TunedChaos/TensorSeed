#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
set -e
if ! getent group tensorseed >/dev/null 2>&1; then
    groupadd --system tensorseed
fi
if ! getent passwd tensorseed >/dev/null 2>&1; then
    shell=/usr/sbin/nologin
    if [ ! -x "$shell" ]; then
        shell=/sbin/nologin
    fi
    if [ ! -x "$shell" ]; then
        shell=/bin/false
    fi
    useradd --system --gid tensorseed --home-dir /var/lib/tensorseed \
        --create-home --shell "$shell" --comment "Tensorseed seeder" tensorseed
fi
