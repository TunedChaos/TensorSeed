// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Building blocks for `tensorseed serve`, the long running seeder (spec
//! section 8.3):
//!
//! - [`http`]: serve verified local revisions over HTTP as a BEP 19 webseed
//!   (`t/<content name>/<path>` and `t/<content name>.torrent`) with byte
//!   ranges, and optionally a mirrored copy of the signed catalog files.
//! - [`health`]: the JSON status document and its localhost endpoint.
//! - [`portmap`]: NAT-PMP and UPnP IGD port mapping for the torrent port.
//! - [`select`]: which catalog revisions to seed (globs, license classes,
//!   latest only, disk budget).
//! - [`ratelimit`]: a shared byte rate limiter.
//!
//! The daemon loop itself lives in the CLI, which owns the configuration,
//! the catalog backends, and the pull pipeline.

pub mod health;
pub mod http;
pub mod portmap;
pub mod ratelimit;
pub mod select;
