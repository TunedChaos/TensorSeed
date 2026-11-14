// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Verify-only Tensorseed: catalog heads and signed entries, manifests,
//! served-weights claims, claim revocations, and signed responses. Covers
//! minisign signatures, the signed-file hash chain, Chia DataLayer inclusion
//! proofs (spec sections 6.8, 6.9, 6.14), and verified serving
//! (docs/plans/verified-serving.md).
//!
//! This crate has no async runtime, no HTTP client, no torrent or serving
//! code, and no OS randomness, and it compiles to WebAssembly.
//! `tensorseed-catalog` re-exports the catalog types and adds the I/O:
//! HTTPS sync, the Chia RPC backend, and coin lookups. `tensorseed-app` and
//! the CLI re-export the claim and response types and add fetching, the
//! seller's keys, and the signing proxy. The browser explorer and the npm
//! package talk to this crate through `tensorseed-wasm`.
//!
//! # Where this fits
//!
//! - [`minisign`]: minisign compatible Ed25519 keys and signatures.
//! - [`signed`]: the signed entry envelope every backend stores.
//! - [`file`](mod@file): heads, deltas, snapshots, and [`file::ChainState`].
//! - [`anchor`]: inclusion proofs, coin id and puzzle hash, and
//!   [`anchor::check_coin_record`].
//! - [`mirrors`]: the signed list of catalog mirrors.
//! - [`manifest`]: signed manifest entries, file pages, and takedowns.
//! - [`claim`]: the `ts.served-weights/1` claim and EIP-191 wallet
//!   signatures.
//! - [`revocation`]: claim revocations, `ts.claim-revocations/1`.
//! - [`response`]: `ts.response-sig/1` signed responses.
//! - [`claim_check`]: every claim check, as a `ts.claim-check/1` report.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod anchor;
pub mod claim;
pub mod claim_check;
pub mod error;
pub mod file;
pub mod manifest;
pub mod minisign;
pub mod mirrors;
pub mod response;
pub mod revocation;
pub mod signed;

pub use error::{Error, Result};
