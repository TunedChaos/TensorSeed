#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
#
# Copy the example config once. Never enable or start the service.
set -e
if [ ! -f /etc/tensorseed/tensorseed.toml ] && [ -f /etc/tensorseed/tensorseed.toml.example ]; then
    cp /etc/tensorseed/tensorseed.toml.example /etc/tensorseed/tensorseed.toml
    chmod 644 /etc/tensorseed/tensorseed.toml
fi
install -d -m 0750 /var/lib/tensorseed
if getent passwd tensorseed >/dev/null 2>&1; then
    chown tensorseed:tensorseed /var/lib/tensorseed
fi
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi
