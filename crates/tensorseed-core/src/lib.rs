// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Core types for Tensorseed: a chain agnostic, verifiable mirror of open
//! Hugging Face models. See `docs/SPEC.md` and <https://tensorseed.dev>.
//!
//! This crate has no network code. It defines the catalog JSON schemas
//! (spec section 6.4), the `ts/v1/` key layout (section 6.3), canonical JSON
//! encoding, streaming hash helpers, and local directory verification.
//!
//! # Where this fits
//!
//! Tensorseed is split into small crates, each with one job:
//!
//! | Crate | Role |
//! |---|---|
//! | `tensorseed-core` | Schemas, key layout, canonical JSON, hashing, local verification (no network). |
//! | `tensorseed-hf` | Hugging Face Hub client: repo info, file trees, license and gating policy. |
//! | `tensorseed-verify` | Minisign signatures, signed-file chain, and chain proofs (no network). |
//! | `tensorseed-catalog` | Signed catalog backends: Chia DataLayer (source of truth) and signed static files (cache). |
//! | `tensorseed-wasm` | WebAssembly bindings used by the browser explorer. |
//! | `tensorseed-torrent` | BitTorrent v2 and hybrid torrent creation, parsing, and the librqbit swarm engine. |
//! | `tensorseed-fetch` | Verified, resumable HTTP downloads from webseeds, mirrors, and the Hub. |
//! | `tensorseed-serve` | Building blocks for the long running seeder: webseed HTTP, health, port mapping, selection. |
//! | `tensorseed-cli` | The `tensorseed` binary that ties everything together. |
//!
//! Data flows one way: a publisher harvests a Hugging Face revision
//! (`tensorseed-hf`), hashes it into a [`manifest::Manifest`], builds a
//! torrent (`tensorseed-torrent`), and signs both into the catalog
//! (`tensorseed-catalog`). Clients and volunteer seeders read the catalog,
//! check it against the chain, and download with `tensorseed-torrent` and
//! `tensorseed-fetch`, verifying every byte against the manifest with the
//! helpers in this crate.
//!
//! # Examples
//!
//! Build catalog keys for a repo and hash some bytes:
//!
//! ```
//! use tensorseed_core::keys::{RepoRef, RepoType};
//! use tensorseed_core::hash::sha256_bytes;
//!
//! let repo = RepoRef::new(RepoType::Model, "org/name")?;
//! let key = repo.manifest_key("0123456789abcdef0123456789abcdef01234567")?;
//! assert_eq!(
//!     key,
//!     "ts/v1/repo/model/org/name/rev/0123456789abcdef0123456789abcdef01234567"
//! );
//! assert_eq!(sha256_bytes(b"").len(), 64);
//! # Ok::<(), tensorseed_core::Error>(())
//! ```
//!
//! Canonical JSON sorts keys at every level, so signatures are stable:
//!
//! ```
//! use tensorseed_core::canonical::to_canonical_string;
//!
//! let v = serde_json::json!({"b": 1, "a": {"d": 2, "c": 3}});
//! assert_eq!(to_canonical_string(&v)?, r#"{"a":{"c":3,"d":2},"b":1}"#);
//! # Ok::<(), tensorseed_core::Error>(())
//! ```

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod canonical;
pub mod error;
pub mod hash;
pub mod keys;
pub mod manifest;
pub mod paths;
pub mod time;
#[cfg(feature = "fs")]
pub mod verify;

pub use error::{Error, Result};

/// Tool identifier written into `Manifest::publisher_tool`.
pub const TOOL_ID: &str = concat!("tensorseed ", env!("CARGO_PKG_VERSION"));
