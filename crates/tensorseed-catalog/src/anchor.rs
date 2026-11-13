// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Chia as the source of truth for the file catalog (spec section 6.14).
//!
//! The publisher writes a reserved DataLayer key, [`FILE_HEAD_KEY`], in every
//! Chia generation. Its value is a signed [`FileAnchor`] naming the file
//! catalog revision and root that generation stands for. After the
//! generation confirms, the publisher asks its DataLayer for a proof of
//! inclusion of that key and publishes it next to `head.json` as
//! [`CHAIN_PROOF_PATH`] (a [`ChainProof`]).
//!
//! A client checks the file catalog against the chain without a wallet or a
//! full node:
//!
//! 1. [`ChainProof::verify`]: the anchor entry carries a valid publisher
//!    signature; hashing the stored key and value up the proof layers gives a
//!    DataLayer root; and the store singleton puzzle hash for
//!    `(store id, root, owner inner puzzle hash)` is computed locally. This
//!    step needs no network and trusts nobody.
//! 2. [`CoinApi::check`]: one or more full node RPC compatible endpoints
//!    (for example a public coinset style API, or your own node) are asked
//!    for the coin named in the proof. The coin id must hash from the
//!    returned parent, puzzle hash, and amount; the puzzle hash must equal
//!    the computed one; and the coin must be unspent (the store's current
//!    state) or spent only within a grace window. This step trusts the
//!    endpoint to report the chain honestly; it cannot forge a root, but it
//!    could hide a newer state or claim a coin does not exist.
//!
//! This is the same check the reference wallet's `verify_proof` performs,
//! with the coin lookup done over HTTP instead of a wallet peer connection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tensorseed_core::canonical::to_canonical_vec;

use crate::minisign::{PublicKey, SecretKey};
use crate::signed::SignedEntry;
use crate::{CatalogError, Result};

/// Reserved DataLayer key holding the signed [`FileAnchor`].
pub const FILE_HEAD_KEY: &str = "ts/v1/file-head";
/// Schema of the [`FileAnchor`] document.
pub const FILE_ANCHOR_SCHEMA: &str = "ts.fileanchor/1";
/// Schema of the published [`ChainProof`].
pub const CHAIN_PROOF_SCHEMA: &str = "ts.chainproof/1";
/// Path of the chain proof inside a file catalog directory.
pub const CHAIN_PROOF_PATH: &str = "chain-proof.json";
/// Largest chain proof a client downloads.
pub const MAX_CHAIN_PROOF_BYTES: usize = 1 << 20;

/// Keys that exist only on the chain side and never in the file catalog,
/// exports, or imports.
pub fn is_reserved_key(key: &str) -> bool {
    key == FILE_HEAD_KEY
}

/// Chia puzzle and module hashes the DataLayer singleton is built from.
/// They are network independent (mainnet and testnet use the same puzzles).
pub mod puzzles {
    /// `singleton_top_layer_v1_1` module hash.
    pub const SINGLETON_TOP_LAYER_V1_1: &str =
        "7faa3253bfddd1e0decb0906b2dc6247bbc4cf608f58345d173adb63e8b47c9f";
    /// Singleton launcher puzzle hash.
    pub const SINGLETON_LAUNCHER: &str =
        "eff07522495060c066f66f32acc2a77e3a3e737aca8baea4d1a64ea4cdc13da9";
    /// `nft_state_layer` module hash (the DataLayer host layer).
    pub const NFT_STATE_LAYER: &str =
        "a04d9f57764f54a43e4030befb4d80026e870519aaa66334aef8304f5d0393c2";
}

type H32 = [u8; 32];

fn sha256(parts: &[&[u8]]) -> H32 {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// CLVM tree hash of an atom.
fn tree_atom(atom: &[u8]) -> H32 {
    sha256(&[&[1], atom])
}

/// CLVM tree hash of a pair.
fn tree_pair(left: &H32, right: &H32) -> H32 {
    sha256(&[&[2], left, right])
}

/// Tree hash of `(a (q . MOD) (c (q . arg1) (c (q . arg2) ... 1)))`, the
/// standard curry, from the module hash and the tree hashes of the
/// arguments (Chia's `curry_and_treehash`).
fn curry_tree_hash(mod_hash: &H32, args: &[H32]) -> H32 {
    let q = tree_atom(&[1]);
    let a = tree_atom(&[2]);
    let c = tree_atom(&[4]);
    let one = tree_atom(&[1]);
    let nil = tree_atom(&[]);
    let mut values = one;
    for arg in args.iter().rev() {
        values = tree_pair(
            &c,
            &tree_pair(&tree_pair(&q, arg), &tree_pair(&values, &nil)),
        );
    }
    tree_pair(
        &a,
        &tree_pair(&tree_pair(&q, mod_hash), &tree_pair(&values, &nil)),
    )
}

fn h32(s: &str, what: &str) -> Result<H32> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let v = hex::decode(s).map_err(|e| CatalogError::Chain(format!("{what}: bad hex: {e}")))?;
    v.try_into()
        .map_err(|_| CatalogError::Chain(format!("{what}: not 32 bytes")))
}

fn const32(s: &str) -> H32 {
    let mut out = [0u8; 32];
    hex::decode_to_slice(s, &mut out).expect("valid constant");
    out
}

/// Normalize a 32 byte hex id: lowercase, no `0x`.
pub fn norm_hex(s: &str) -> String {
    s.strip_prefix("0x").unwrap_or(s).to_ascii_lowercase()
}

/// Puzzle hash of a DataLayer store singleton coin whose current root is
/// `root` and whose owner inner puzzle hash is `inner_puzzle_hash`. Mirrors
/// `create_host_fullpuz` in the reference client
/// (`chia/wallet/db_wallet/db_wallet_puzzles.py`).
pub fn datalayer_puzzle_hash(store_id: &H32, root: &H32, inner_puzzle_hash: &H32) -> H32 {
    let nft_state = const32(puzzles::NFT_STATE_LAYER);
    let singleton = const32(puzzles::SINGLETON_TOP_LAYER_V1_1);
    let launcher = const32(puzzles::SINGLETON_LAUNCHER);
    // Metadata is `(root . ())`; the metadata updater is `11` (ACS_MU).
    let metadata = tree_pair(&tree_atom(root), &tree_atom(&[]));
    let acs_mu_ph = tree_atom(&[11]);
    let host = curry_tree_hash(
        &nft_state,
        &[
            tree_atom(&nft_state),
            metadata,
            tree_atom(&acs_mu_ph),
            *inner_puzzle_hash,
        ],
    );
    let singleton_struct = tree_pair(
        &tree_atom(&singleton),
        &tree_pair(&tree_atom(store_id), &tree_atom(&launcher)),
    );
    curry_tree_hash(&singleton, &[singleton_struct, host])
}

/// Chia coin id: `sha256(parent || puzzle_hash || amount)` with the amount
/// as a minimal signed big endian integer (CLVM int encoding).
pub fn coin_id(parent: &H32, puzzle_hash: &H32, amount: u64) -> H32 {
    let mut bytes = amount.to_be_bytes().to_vec();
    while bytes.first() == Some(&0) {
        bytes.remove(0);
    }
    if bytes.first().is_some_and(|b| b & 0x80 != 0) {
        bytes.insert(0, 0);
    }
    sha256(&[parent, puzzle_hash, &bytes])
}

/// DataLayer leaf hash of a key and value (`leaf_hash` in the reference
/// client): the tree hash of the pair `(key . value)`.
pub fn datalayer_leaf_hash(key: &[u8], value: &[u8]) -> H32 {
    tree_pair(&tree_atom(key), &tree_atom(value))
}

/// Hash a leaf up the proof layers to the DataLayer root, checking every
/// layer's combined hash on the way (`ProofOfInclusion.valid` in the
/// reference client).
pub fn fold_proof(leaf: H32, layers: &[ProofLayer]) -> Result<H32> {
    let mut cur = leaf;
    for (i, l) in layers.iter().enumerate() {
        let other = h32(&l.other_hash, "other_hash")?;
        cur = match l.other_hash_side {
            0 => tree_pair(&other, &cur),
            1 => tree_pair(&cur, &other),
            s => {
                return Err(CatalogError::Chain(format!(
                    "proof layer {i}: bad side {s}"
                )));
            }
        };
        if h32(&l.combined_hash, "combined_hash")? != cur {
            return Err(CatalogError::Chain(format!(
                "proof layer {i} does not hash to its combined hash"
            )));
        }
    }
    Ok(cur)
}

/// The value under [`FILE_HEAD_KEY`]: the file catalog revision and root a
/// Chia generation stands for. It deliberately leaves out the head's expiry
/// and snapshot pointer, so re-signing `head.json` or writing a snapshot
/// needs no chain transaction; `root` already commits to every entry and to
/// the whole history (spec 6.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileAnchor {
    /// Always [`FILE_ANCHOR_SCHEMA`].
    pub schema: String,
    /// Logical catalog name.
    pub catalog: String,
    /// File catalog revision.
    pub revision: u64,
    /// File catalog root at `revision` (hex).
    pub root: String,
}

impl FileAnchor {
    /// New anchor document.
    pub fn new(catalog: &str, revision: u64, root: &str) -> Self {
        Self {
            schema: FILE_ANCHOR_SCHEMA.into(),
            catalog: catalog.into(),
            revision,
            root: norm_hex(root),
        }
    }

    /// Sign it as the [`FILE_HEAD_KEY`] entry.
    pub fn sign(&self, sk: &SecretKey) -> Result<SignedEntry> {
        SignedEntry::sign(sk, &self.catalog, FILE_HEAD_KEY, &to_canonical_vec(self)?)
    }

    /// Verify a [`FILE_HEAD_KEY`] entry and parse its anchor.
    pub fn from_entry(entry: &SignedEntry, pk: &PublicKey, catalog: &str) -> Result<Self> {
        if entry.key != FILE_HEAD_KEY {
            return Err(CatalogError::Chain(format!(
                "anchor entry is for key {:?}, expected {FILE_HEAD_KEY}",
                entry.key
            )));
        }
        entry.verify(pk, catalog)?;
        let a: FileAnchor = serde_json::from_str(&entry.value)?;
        if a.schema != FILE_ANCHOR_SCHEMA {
            return Err(CatalogError::Chain(format!(
                "unknown anchor schema {:?}",
                a.schema
            )));
        }
        if a.catalog != catalog {
            return Err(CatalogError::Chain(format!(
                "anchor is for catalog {:?}, expected {catalog:?}",
                a.catalog
            )));
        }
        h32(&a.root, "anchor root")?;
        Ok(a)
    }

    /// True when this anchor names exactly `(revision, root)`.
    pub fn matches(&self, revision: u64, root: &str) -> bool {
        self.revision == revision && self.root == norm_hex(root)
    }
}

/// One step of a DataLayer proof of inclusion, as returned by `get_proof`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofLayer {
    /// 0: the sibling is on the left; 1: on the right.
    pub other_hash_side: u8,
    /// Sibling hash (hex).
    pub other_hash: String,
    /// Hash of this node and its sibling (hex).
    pub combined_hash: String,
}

/// Published `chain-proof.json`: everything a client needs to check the
/// file catalog against the chain except the coin lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainProof {
    /// Always [`CHAIN_PROOF_SCHEMA`].
    pub schema: String,
    /// DataLayer store id (the singleton launcher id), hex.
    pub store_id: String,
    /// Store singleton coin for the proven root, hex.
    pub coin_id: String,
    /// Owner inner puzzle hash at that coin, hex.
    pub inner_puzzle_hash: String,
    /// Leaf hash of the anchor key and value, hex.
    pub node_hash: String,
    /// Path from the leaf to the root.
    pub layers: Vec<ProofLayer>,
    /// The signed [`FILE_HEAD_KEY`] entry; its canonical bytes are the
    /// stored DataLayer value.
    pub entry: SignedEntry,
}

/// What a proof establishes offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedProof {
    /// Store id, hex.
    pub store_id: String,
    /// DataLayer root the anchor is included in, hex.
    pub root: String,
    /// Singleton coin id, hex.
    pub coin_id: String,
    /// Puzzle hash that coin must have, hex.
    pub puzzle_hash: String,
    /// The signed anchor.
    pub anchor: FileAnchor,
}

impl ChainProof {
    /// Build from a DataLayer `get_proof` response for [`FILE_HEAD_KEY`]
    /// and the stored entry.
    pub fn from_get_proof(resp: &Value, entry: SignedEntry) -> Result<Self> {
        let bad = |m: &str| CatalogError::Backend(format!("get_proof response: {m}"));
        let proof = resp.get("proof").ok_or_else(|| bad("no proof"))?;
        let s = |v: &Value, k: &str| -> Result<String> {
            v.get(k)
                .and_then(Value::as_str)
                .map(norm_hex)
                .ok_or_else(|| bad(&format!("no {k}")))
        };
        let sp = proof
            .get("store_proofs")
            .ok_or_else(|| bad("no store_proofs"))?;
        let proofs = sp
            .get("proofs")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("no proofs"))?;
        let [one] = proofs.as_slice() else {
            return Err(bad("expected exactly one proof"));
        };
        let mut layers = Vec::new();
        for l in one
            .get("layers")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("no layers"))?
        {
            let side = match l.get("other_hash_side") {
                Some(Value::Number(n)) => n.as_u64().unwrap_or(u64::MAX),
                Some(Value::String(t)) if t.eq_ignore_ascii_case("left") => 0,
                Some(Value::String(t)) if t.eq_ignore_ascii_case("right") => 1,
                _ => u64::MAX,
            };
            layers.push(ProofLayer {
                other_hash_side: u8::try_from(side).map_err(|_| bad("bad other_hash_side"))?,
                other_hash: s(l, "other_hash")?,
                combined_hash: s(l, "combined_hash")?,
            });
        }
        Ok(Self {
            schema: CHAIN_PROOF_SCHEMA.into(),
            store_id: s(sp, "store_id")?,
            coin_id: s(proof, "coin_id")?,
            inner_puzzle_hash: s(proof, "inner_puzzle_hash")?,
            node_hash: s(one, "node_hash")?,
            layers,
            entry,
        })
    }

    /// Canonical JSON bytes for publishing.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(to_canonical_vec(self)?)
    }

    /// Parse published bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CHAIN_PROOF_BYTES {
            return Err(CatalogError::Chain("chain proof too large".into()));
        }
        let p: Self = serde_json::from_slice(bytes)?;
        if p.schema != CHAIN_PROOF_SCHEMA {
            return Err(CatalogError::Chain(format!(
                "unknown chain proof schema {:?}",
                p.schema
            )));
        }
        Ok(p)
    }

    /// Offline verification: signature, inclusion, and the puzzle hash the
    /// store coin must have. Needs no network.
    pub fn verify(&self, pk: &PublicKey, catalog: &str, store_id: &str) -> Result<VerifiedProof> {
        let want = norm_hex(store_id);
        if norm_hex(&self.store_id) != want {
            return Err(CatalogError::Chain(format!(
                "proof is for store {}, expected {want}",
                self.store_id
            )));
        }
        let store = h32(&want, "store id")?;
        let anchor = FileAnchor::from_entry(&self.entry, pk, catalog)?;
        let value = self.entry.to_bytes()?;
        let leaf = datalayer_leaf_hash(FILE_HEAD_KEY.as_bytes(), &value);
        if h32(&self.node_hash, "node_hash")? != leaf {
            return Err(CatalogError::Chain(
                "proof leaf does not hash from the anchor key and value".into(),
            ));
        }
        let cur = fold_proof(leaf, &self.layers)?;
        let inner = h32(&self.inner_puzzle_hash, "inner_puzzle_hash")?;
        h32(&self.coin_id, "coin_id")?;
        let ph = datalayer_puzzle_hash(&store, &cur, &inner);
        Ok(VerifiedProof {
            store_id: want,
            root: hex::encode(cur),
            coin_id: norm_hex(&self.coin_id),
            puzzle_hash: hex::encode(ph),
            anchor,
        })
    }
}

/// A coin record as reported by a full node RPC (`get_coin_record_by_name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinRecord {
    /// Parent coin id, hex.
    pub parent: String,
    /// Puzzle hash, hex.
    pub puzzle_hash: String,
    /// Amount in mojos.
    pub amount: u64,
    /// Whether the coin is spent.
    pub spent: bool,
    /// Height it was spent at (0 when unspent).
    pub spent_block_index: u64,
    /// Height it was created at.
    pub confirmed_block_index: u64,
}

/// Where the proven state sits on chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoinState {
    /// The coin is unspent: this is the store's current root.
    Current,
    /// The coin was spent `blocks_ago` blocks ago, within the grace window:
    /// the store moved on very recently (caches may lag behind).
    RecentlySpent {
        /// Blocks since the spend.
        blocks_ago: u64,
    },
    /// The coin was spent longer ago than the grace window: a newer chain
    /// state exists and this proof is stale.
    Stale {
        /// Blocks since the spend.
        blocks_ago: u64,
    },
}

/// Result of a coin lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinCheck {
    /// Where the proven root sits.
    pub state: CoinState,
    /// Height the coin was created at.
    pub confirmed_block_index: u64,
    /// Endpoints that answered.
    pub answered: Vec<String>,
    /// Endpoints that failed (unreachable or malformed answers).
    pub skipped: Vec<String>,
}

/// Read only client for full node RPC compatible HTTP endpoints, such as a
/// public coinset style API or a local full node exposed on localhost.
#[derive(Debug, Clone)]
pub struct CoinApi {
    http: reqwest::Client,
    urls: Vec<String>,
}

impl CoinApi {
    /// Client for `urls` (each a base such as `https://api.example.org`).
    pub fn new(urls: Vec<String>, http: reqwest::Client) -> Self {
        Self { http, urls }
    }

    /// Configured endpoints.
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    async fn call(&self, base: &str, method: &str, body: Value) -> Result<Value> {
        let url = format!("{}/{method}", base.trim_end_matches('/'));
        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() && v.get("success").is_none() {
            return Err(CatalogError::Backend(format!("{url}: HTTP {status}")));
        }
        Ok(v)
    }

    /// The coin record, or `None` when the endpoint says it does not exist.
    pub async fn coin_record(&self, base: &str, coin: &str) -> Result<Option<CoinRecord>> {
        let v = self
            .call(
                base,
                "get_coin_record_by_name",
                json!({ "name": format!("0x{}", norm_hex(coin)) }),
            )
            .await?;
        if v.get("success").and_then(Value::as_bool) != Some(true) {
            let err = v.get("error").and_then(Value::as_str).unwrap_or("");
            if err.to_ascii_lowercase().contains("not found") {
                return Ok(None);
            }
            return Err(CatalogError::Backend(format!(
                "{base}: get_coin_record_by_name failed: {err}"
            )));
        }
        let r = v
            .get("coin_record")
            .ok_or_else(|| CatalogError::Backend(format!("{base}: no coin_record")))?;
        if r.is_null() {
            return Ok(None);
        }
        let c = r
            .get("coin")
            .ok_or_else(|| CatalogError::Backend(format!("{base}: no coin")))?;
        let s = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(norm_hex)
                .ok_or_else(|| CatalogError::Backend(format!("{base}: no {k}")))
        };
        let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Ok(Some(CoinRecord {
            parent: s(c, "parent_coin_info")?,
            puzzle_hash: s(c, "puzzle_hash")?,
            amount: c
                .get("amount")
                .and_then(Value::as_u64)
                .ok_or_else(|| CatalogError::Backend(format!("{base}: no amount")))?,
            spent: r.get("spent").and_then(Value::as_bool).unwrap_or(false)
                || n(r, "spent_block_index") > 0,
            spent_block_index: n(r, "spent_block_index"),
            confirmed_block_index: n(r, "confirmed_block_index"),
        }))
    }

    /// Current peak height.
    pub async fn peak_height(&self, base: &str) -> Result<u64> {
        let v = self.call(base, "get_blockchain_state", json!({})).await?;
        v.pointer("/blockchain_state/peak/height")
            .and_then(Value::as_u64)
            .ok_or_else(|| CatalogError::Backend(format!("{base}: no peak height")))
    }

    /// Look the proven coin up on every endpoint. Any endpoint that returns
    /// a coin that does not match the proof, or says the coin does not
    /// exist, fails the check (`Chain` error). Endpoints that cannot be
    /// reached are skipped; if none answers the result is
    /// [`CatalogError::ChainUnreachable`]. When endpoints disagree about
    /// whether the coin is spent, the most conservative answer wins.
    pub async fn check(&self, proof: &VerifiedProof, grace_blocks: u64) -> Result<CoinCheck> {
        if self.urls.is_empty() {
            return Err(CatalogError::Config(
                "no chain endpoints configured (chia.coin_api_urls)".into(),
            ));
        }
        let want_id = h32(&proof.coin_id, "coin_id")?;
        let mut out = CoinCheck {
            state: CoinState::Current,
            confirmed_block_index: 0,
            answered: Vec::new(),
            skipped: Vec::new(),
        };
        for base in &self.urls {
            let rec = match self.coin_record(base, &proof.coin_id).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return Err(CatalogError::Chain(format!(
                        "{base}: store coin {} does not exist on chain",
                        proof.coin_id
                    )));
                }
                Err(e) => {
                    out.skipped.push(format!("{base}: {e}"));
                    continue;
                }
            };
            let parent = h32(&rec.parent, "parent_coin_info")?;
            let ph = h32(&rec.puzzle_hash, "puzzle_hash")?;
            if coin_id(&parent, &ph, rec.amount) != want_id {
                return Err(CatalogError::Chain(format!(
                    "{base}: returned a coin record that does not hash to {}",
                    proof.coin_id
                )));
            }
            if rec.puzzle_hash != proof.puzzle_hash {
                return Err(CatalogError::Chain(format!(
                    "{base}: store coin {} has puzzle hash {}, but the proof needs {} \
                     (the proof's root is not this store's root at that coin)",
                    proof.coin_id, rec.puzzle_hash, proof.puzzle_hash
                )));
            }
            if rec.amount % 2 != 1 {
                return Err(CatalogError::Chain(format!(
                    "{base}: coin {} has an even amount; not a singleton",
                    proof.coin_id
                )));
            }
            out.confirmed_block_index = rec.confirmed_block_index;
            if rec.spent {
                let peak = match self.peak_height(base).await {
                    Ok(p) => p,
                    Err(e) => {
                        out.skipped.push(format!("{base}: {e}"));
                        continue;
                    }
                };
                let blocks_ago = peak.saturating_sub(rec.spent_block_index);
                let st = if blocks_ago <= grace_blocks {
                    CoinState::RecentlySpent { blocks_ago }
                } else {
                    CoinState::Stale { blocks_ago }
                };
                out.state = worse(&out.state, st);
            }
            out.answered.push(base.clone());
        }
        if out.answered.is_empty() {
            return Err(CatalogError::ChainUnreachable(out.skipped.join("; ")));
        }
        Ok(out)
    }
}

fn worse(a: &CoinState, b: CoinState) -> CoinState {
    fn rank(s: &CoinState) -> u8 {
        match s {
            CoinState::Current => 0,
            CoinState::RecentlySpent { .. } => 1,
            CoinState::Stale { .. } => 2,
        }
    }
    if rank(&b) >= rank(a) { b } else { a.clone() }
}

/// The keys that must change on the chain side so the store holds exactly
/// `entries` plus a fresh anchor: returns the deletes (chain keys under
/// `ts/` that are neither in `entries` nor reserved).
pub fn chain_deletes(
    chain_keys: &[String],
    entries: &BTreeMap<String, SignedEntry>,
) -> Vec<String> {
    chain_keys
        .iter()
        .filter(|k| !is_reserved_key(k) && !entries.contains_key(*k))
        .cloned()
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::minisign::{PublicKey, SecretKey};

    // A real testnet11 DataLayer store (created by the Tensorseed testnet
    // run, spec 6.13): generation 1 root, owner inner puzzle hash, and the
    // singleton coin as reported on chain.
    const STORE: &str = "3bc1ab7705c525eea55ba6f76db8baff25de0bc282a4b626c6f7b070de5c5d89";
    const ROOT1: &str = "dbf3a4f4eba03f77fd549a7df21512ee2990c5273699f762530df49ba436ea38";
    const INNER1: &str = "dde6ead54478217121c72f3be11f69bbb368fa7d471cb27622b9882e81300705";
    const PH1: &str = "7b6691b65b9e0fd350e815b1f3f912bfb18a21a17ba375f7603683cf52b5eead";
    const PARENT1: &str = "263611b1acb455e626cd48a2257a3c0b406cd7cdef14953bf500d65e6093d79b";
    const COIN1: &str = "ba3d4ca3f6c5c53e564d50f96f785a7588d318e372aed5bfbeb565f676f290f0";
    // Generation 0 (empty root) of the same store.
    const INNER0: &str = "f177f865c13c8d83bd6b01005b801d24a59e4153d6ecb78e5db497d259e15ecb";
    const PH0: &str = "0806911388049ba83ee0c6fda4d7752c51921bb97eab29158818a68f5b6df4c1";

    fn b(s: &str) -> H32 {
        h32(s, "test").unwrap()
    }

    #[test]
    fn puzzle_hash_matches_real_testnet_singletons() {
        let ph = datalayer_puzzle_hash(&b(STORE), &b(ROOT1), &b(INNER1));
        assert_eq!(hex::encode(ph), PH1);
        let ph0 = datalayer_puzzle_hash(&b(STORE), &[0; 32], &b(INNER0));
        assert_eq!(hex::encode(ph0), PH0);
        // Any other root gives a different puzzle hash.
        let mut other = b(ROOT1);
        other[0] ^= 1;
        assert_ne!(
            hex::encode(datalayer_puzzle_hash(&b(STORE), &other, &b(INNER1))),
            PH1
        );
    }

    #[test]
    fn coin_id_matches_real_testnet_coin() {
        assert_eq!(hex::encode(coin_id(&b(PARENT1), &b(PH1), 1)), COIN1);
        // Amount encoding: minimal signed big endian.
        let p = [7u8; 32];
        let ph = [9u8; 32];
        assert_eq!(coin_id(&p, &ph, 0), sha256(&[&p, &ph, &[]]));
        assert_eq!(coin_id(&p, &ph, 128), sha256(&[&p, &ph, &[0, 128]]));
        assert_eq!(coin_id(&p, &ph, 256), sha256(&[&p, &ph, &[1, 0]]));
    }

    /// Build a small DataLayer style tree around the anchor leaf and return
    /// a proof for it, as `get_proof` would.
    pub(crate) fn fake_get_proof(
        entry: &SignedEntry,
        inner: &str,
        coin: &str,
        store: &str,
    ) -> (Value, String) {
        let leaf = datalayer_leaf_hash(FILE_HEAD_KEY.as_bytes(), &entry.to_bytes().unwrap());
        let sib1 = datalayer_leaf_hash(b"ts/v1/catalog", b"x");
        let sib2 = tree_pair(
            &datalayer_leaf_hash(b"a", b"1"),
            &datalayer_leaf_hash(b"b", b"2"),
        );
        let l1 = tree_pair(&sib1, &leaf); // sibling on the left
        let root = tree_pair(&l1, &sib2); // sibling on the right
        let resp = json!({
            "proof": {
                "coin_id": format!("0x{coin}"),
                "inner_puzzle_hash": format!("0x{inner}"),
                "store_proofs": {
                    "store_id": format!("0x{store}"),
                    "proofs": [{
                        "key_clvm_hash": format!("0x{}", hex::encode(tree_atom(FILE_HEAD_KEY.as_bytes()))),
                        "value_clvm_hash": "0x00",
                        "node_hash": format!("0x{}", hex::encode(leaf)),
                        "layers": [
                            {"other_hash_side": 0, "other_hash": format!("0x{}", hex::encode(sib1)), "combined_hash": format!("0x{}", hex::encode(l1))},
                            {"other_hash_side": 1, "other_hash": format!("0x{}", hex::encode(sib2)), "combined_hash": format!("0x{}", hex::encode(root))}
                        ]
                    }]
                }
            },
            "success": true
        });
        (resp, hex::encode(root))
    }

    fn key() -> SecretKey {
        SecretKey::from_seed(&[42; 32])
    }

    #[test]
    fn proof_round_trip_and_tamper_detection() {
        let sk = key();
        let pk = sk.public_key();
        let entry = FileAnchor::new("main", 7, &"ab".repeat(32))
            .sign(&sk)
            .unwrap();
        let (resp, root) = fake_get_proof(&entry, INNER1, COIN1, STORE);
        let p = ChainProof::from_get_proof(&resp, entry.clone()).unwrap();
        let p = ChainProof::from_bytes(&p.to_bytes().unwrap()).unwrap();
        let v = p.verify(&pk, "main", STORE).unwrap();
        assert_eq!(v.root, root);
        assert!(v.anchor.matches(7, &"ab".repeat(32)));
        assert_eq!(
            v.puzzle_hash,
            hex::encode(datalayer_puzzle_hash(&b(STORE), &b(&root), &b(INNER1)))
        );

        // Wrong store, wrong catalog, wrong key.
        assert!(p.verify(&pk, "main", &"00".repeat(32)).is_err());
        assert!(p.verify(&pk, "other", STORE).is_err());
        assert!(
            p.verify(&SecretKey::from_seed(&[1; 32]).public_key(), "main", STORE)
                .is_err()
        );

        // A tampered anchor no longer matches the leaf.
        let mut t = p.clone();
        t.entry = FileAnchor::new("main", 8, &"ab".repeat(32))
            .sign(&sk)
            .unwrap();
        assert!(matches!(
            t.verify(&pk, "main", STORE),
            Err(CatalogError::Chain(_))
        ));
        // A tampered layer.
        let mut t = p.clone();
        t.layers[1].other_hash = "cd".repeat(32);
        assert!(t.verify(&pk, "main", STORE).is_err());
        let mut t = p.clone();
        t.layers[0].other_hash_side = 1;
        assert!(t.verify(&pk, "main", STORE).is_err());
        // An entry under another key cannot pose as the anchor.
        let mut t = p;
        t.entry = SignedEntry::sign(&sk, "main", "ts/v1/catalog", entry.value.as_bytes()).unwrap();
        assert!(t.verify(&pk, "main", STORE).is_err());
    }

    /// A proof of inclusion for `ts/v1/catalog` in that testnet11 store at
    /// generation 1, produced by the reference client's DataLayer code from
    /// its local copy of the store (public chain data).
    const REAL_PROOF: &str = include_str!("../tests/fixtures/testnet11-proof.json");

    fn real_inclusion() -> (Vec<u8>, Vec<u8>, String, Vec<ProofLayer>) {
        let v: Value = serde_json::from_str(REAL_PROOF).unwrap();
        let layers = serde_json::from_value(v["layers"].clone()).unwrap();
        (
            hex::decode(v["key"].as_str().unwrap()).unwrap(),
            hex::decode(v["value"].as_str().unwrap()).unwrap(),
            v["node_hash"].as_str().unwrap().to_string(),
            layers,
        )
    }

    #[test]
    fn real_datalayer_proof_folds_to_the_on_chain_root() {
        let (key, value, node, layers) = real_inclusion();
        let leaf = datalayer_leaf_hash(&key, &value);
        assert_eq!(hex::encode(leaf), node);
        let root = fold_proof(leaf, &layers).unwrap();
        assert_eq!(hex::encode(root), ROOT1);
        assert_eq!(
            hex::encode(datalayer_puzzle_hash(&b(STORE), &root, &b(INNER1))),
            PH1
        );
        // The stored value is the signed catalog entry of the live catalog.
        let e = SignedEntry::from_bytes(&value).unwrap();
        e.verify(
            &PublicKey::parse("RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd").unwrap(),
            "tensorseed",
        )
        .unwrap();
        let mut bad = layers.clone();
        bad[0].other_hash_side ^= 1;
        assert!(fold_proof(leaf, &bad).is_err());
    }

    /// Live: look the real testnet11 store coin up on a public coinset style
    /// API. Run with `cargo test -p tensorseed-catalog -- --ignored live_`.
    #[tokio::test]
    #[ignore = "needs network"]
    async fn live_testnet11_store_coin_matches_the_proof() {
        let (key, value, _, layers) = real_inclusion();
        let root = fold_proof(datalayer_leaf_hash(&key, &value), &layers).unwrap();
        let ph = datalayer_puzzle_hash(&b(STORE), &root, &b(INNER1));
        let p = VerifiedProof {
            store_id: STORE.into(),
            root: hex::encode(root),
            coin_id: COIN1.into(),
            puzzle_hash: hex::encode(ph),
            anchor: FileAnchor::new("tensorseed", 1, ROOT1),
        };
        let url = std::env::var("TS_TESTNET_COIN_API")
            .unwrap_or_else(|_| "https://testnet11.api.coinset.org".into());
        let c = api(vec![url]).check(&p, 192).await.unwrap();
        eprintln!(
            "testnet11 store coin {COIN1}: {:?}, created at {}",
            c.state, c.confirmed_block_index
        );
        assert_eq!(c.confirmed_block_index, 4956663);
    }

    async fn coin_server(record: Value, peak: u64) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/get_coin_record_by_name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(record))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/get_blockchain_state"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"success": true, "blockchain_state": {"peak": {"height": peak}}}),
            ))
            .mount(&s)
            .await;
        s
    }

    fn real_proof() -> VerifiedProof {
        VerifiedProof {
            store_id: STORE.into(),
            root: ROOT1.into(),
            coin_id: COIN1.into(),
            puzzle_hash: PH1.into(),
            anchor: FileAnchor::new("main", 1, &"ab".repeat(32)),
        }
    }

    fn record(ph: &str, spent_at: u64) -> Value {
        // Shape as returned by a public coinset style API on testnet11.
        json!({"coin_record": {
            "coin": {"parent_coin_info": format!("0x{PARENT1}"), "puzzle_hash": format!("0x{ph}"), "amount": 1},
            "coinbase": false, "confirmed_block_index": 4956663,
            "spent": spent_at > 0, "spent_block_index": spent_at, "timestamp": 1794526151
        }, "success": true})
    }

    fn api(urls: Vec<String>) -> CoinApi {
        CoinApi::new(urls, reqwest::Client::new())
    }

    #[tokio::test]
    async fn coin_check_states() {
        let p = real_proof();
        let live = coin_server(record(PH1, 0), 5_000_000).await;
        let c = api(vec![live.uri()]).check(&p, 100).await.unwrap();
        assert_eq!(c.state, CoinState::Current);
        assert_eq!(c.confirmed_block_index, 4956663);

        let recent = coin_server(record(PH1, 4_999_950), 5_000_000).await;
        let c = api(vec![recent.uri()]).check(&p, 100).await.unwrap();
        assert_eq!(c.state, CoinState::RecentlySpent { blocks_ago: 50 });
        let c = api(vec![live.uri(), recent.uri()])
            .check(&p, 10)
            .await
            .unwrap();
        assert_eq!(
            c.state,
            CoinState::Stale { blocks_ago: 50 },
            "most conservative answer wins"
        );
        assert_eq!(c.answered.len(), 2);
    }

    #[tokio::test]
    async fn coin_check_rejects_mismatches_and_reports_unreachable() {
        let p = real_proof();
        // A coin whose puzzle hash commits to another root: forged proof.
        let wrong = coin_server(record(PH0, 0), 10).await;
        let e = api(vec![wrong.uri()]).check(&p, 100).await.unwrap_err();
        assert!(matches!(e, CatalogError::Chain(_)), "{e}");
        // Not on chain at all.
        let none = coin_server(
            json!({"success": false, "error": "Coin record 0x00 not found"}),
            10,
        )
        .await;
        assert!(matches!(
            api(vec![none.uri()]).check(&p, 100).await,
            Err(CatalogError::Chain(_))
        ));
        // A record that does not hash to the requested coin id.
        let mut r = record(PH1, 0);
        r["coin_record"]["coin"]["amount"] = json!(3);
        let liar = coin_server(r, 10).await;
        assert!(matches!(
            api(vec![liar.uri()]).check(&p, 100).await,
            Err(CatalogError::Chain(_))
        ));
        // Nobody answers.
        let e = api(vec!["http://127.0.0.1:9".into()])
            .check(&p, 100)
            .await
            .unwrap_err();
        assert!(matches!(e, CatalogError::ChainUnreachable(_)), "{e}");
        // One dead endpoint is skipped when another answers.
        let live = coin_server(record(PH1, 0), 10).await;
        let c = api(vec!["http://127.0.0.1:9".into(), live.uri()])
            .check(&p, 100)
            .await
            .unwrap();
        assert_eq!(c.skipped.len(), 1);
        assert!(matches!(
            api(vec![]).check(&p, 100).await,
            Err(CatalogError::Config(_))
        ));
    }

    #[test]
    fn deletes_skip_reserved_and_present_keys() {
        let sk = key();
        let mut entries = BTreeMap::new();
        let e = SignedEntry::sign(&sk, "main", "ts/v1/a", b"{}").unwrap();
        entries.insert(e.key.clone(), e);
        let keys = vec![
            "ts/v1/a".to_string(),
            "ts/v1/b".to_string(),
            FILE_HEAD_KEY.to_string(),
        ];
        assert_eq!(chain_deletes(&keys, &entries), vec!["ts/v1/b".to_string()]);
    }
}
