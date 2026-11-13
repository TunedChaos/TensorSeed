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
//!
//! # Where this fits
//!
//! `tensorseed serve` is how volunteers add redundancy: it syncs the
//! chain checked catalog, picks revisions with [`select::Rules`], downloads
//! and verifies them, and seeds them over BitTorrent, optionally also over
//! HTTP. See `docs/RUN-A-MIRROR.md` for the operator guide.
//!
//! # Examples
//!
//! Selection rules decide what a seeder keeps:
//!
//! ```
//! use tensorseed_serve::select::{glob_match, Rules};
//!
//! assert!(glob_match("TunedChaos/*", "TunedChaos/some-model"));
//! let rules = Rules {
//!     select: vec!["org/*".into()],
//!     exclude: vec!["org/huge-*".into()],
//!     ..Rules::default()
//! };
//! assert_eq!(rules.rank("org/small"), Some(0));
//! assert_eq!(rules.rank("org/huge-model"), None);
//! ```

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod health;
pub mod http;
pub mod portmap;
pub mod ratelimit;
pub mod select;
