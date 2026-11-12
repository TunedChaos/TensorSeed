// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos LLC

//! Core types for Tensorseed: a chain agnostic, verifiable mirror of open
//! Hugging Face models. See `docs/SPEC.md` and <https://tensorseed.dev>.
//!
//! This crate has no network code. It defines the catalog JSON schemas
//! (spec section 6.4), the `ts/v1/` key layout (section 6.3), canonical JSON
//! encoding, and a streaming SHA-256 helper.

pub mod canonical;
pub mod error;
pub mod hash;
pub mod keys;
pub mod manifest;

pub use error::{Error, Result};

/// Tool identifier written into `Manifest::publisher_tool`.
pub const TOOL_ID: &str = concat!("tensorseed ", env!("CARGO_PKG_VERSION"));
