// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Tensorseed application library: the configuration, catalog access,
//! mirror selection, and torrent helpers shared by the `tensorseed` CLI and
//! Tensorseed Desktop. Nothing here parses command lines; front ends own
//! their own arguments and output. See `docs/plans/0.3.0-desktop.md`.

pub mod backend;
pub mod chain;
pub mod challenge;
pub mod config;
pub mod doctor;
pub mod follow;
pub mod hf;
pub mod inference;
pub mod mirrors;
pub mod pull;
pub mod serve_config;
pub mod sign_proxy;
pub mod signing;
pub mod swarm;
pub mod torrents;
