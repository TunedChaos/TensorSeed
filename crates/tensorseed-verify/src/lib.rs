// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Pure catalog verification: minisign signatures, the signed-file hash
//! chain, and Chia DataLayer inclusion proofs (spec sections 6.8, 6.9, 6.14).
//!
//! This crate has no async runtime, no HTTP client, and no OS randomness.
//! `tensorseed-catalog` re-exports the same types and adds the I/O: HTTPS
//! sync, the Chia RPC backend, and coin lookups. The browser explorer talks
//! to this crate through `tensorseed-wasm`.
//!
//! # Where this fits
//!
//! - [`minisign`]: minisign compatible Ed25519 keys and signatures.
//! - [`signed`]: the signed entry envelope every backend stores.
//! - [`file`]: heads, deltas, snapshots, and [`file::ChainState`].
//! - [`anchor`]: inclusion proofs, coin id and puzzle hash, and
//!   [`anchor::check_coin_record`].
//! - [`mirrors`]: the signed list of catalog mirrors.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod anchor;
pub mod error;
pub mod file;
pub mod minisign;
pub mod mirrors;
pub mod signed;

pub use error::{Error, Result};
