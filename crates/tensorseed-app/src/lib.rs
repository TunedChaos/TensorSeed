// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Tensorseed application library: the configuration, catalog access,
//! mirror selection, and torrent helpers shared by the `tensorseed` CLI and
//! Tensorseed Desktop. Nothing here parses command lines; front ends own
//! their own arguments and output. See `docs/plans/0.3.0-desktop.md`.

pub mod config;
pub mod follow;
pub mod serve_config;
