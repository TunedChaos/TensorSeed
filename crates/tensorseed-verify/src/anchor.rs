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
use crate::{Error as CatalogError, Result};

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

/// Parse a 32 byte hex id, optional `0x` prefix.
///
/// # Errors
///
/// [`Error::Chain`] when the string is not 32 bytes of hex.
pub fn h32(s: &str, what: &str) -> Result<H32> {
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
///
/// # Errors
///
/// [`CatalogError::Chain`] when a layer's recorded combined hash does not match
/// the hash computed from its children.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if the anchor cannot be encoded as canonical
    /// JSON, or [`CatalogError::Signature`] if signing fails.
    pub fn sign(&self, sk: &SecretKey) -> Result<SignedEntry> {
        SignedEntry::sign(sk, &self.catalog, FILE_HEAD_KEY, &to_canonical_vec(self)?)
    }

    /// Verify a [`FILE_HEAD_KEY`] entry and parse its anchor.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if the entry is not signed by `pk` for
    /// `catalog` or carries the wrong key, and [`CatalogError::Json`] or
    /// [`CatalogError::Chain`] if the value is not a well formed anchor.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Backend`] or [`CatalogError::Chain`] when the response
    /// is missing fields, carries malformed hex, or proves a different key or
    /// value than `entry`.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if canonical JSON encoding fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(to_canonical_vec(self)?)
    }

    /// Parse published bytes.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Json`] if `bytes` is not a valid chain proof document,
    /// or [`CatalogError::Chain`] if its schema is not recognised.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] for a bad or foreign signature, and
    /// [`CatalogError::Chain`] when the inclusion proof does not fold to the
    /// claimed root or the store id does not match `store_id`.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl CoinRecord {
    /// Parse a `get_coin_record_by_name` JSON body.
    ///
    /// Returns `Ok(None)` when the endpoint says the coin is not on chain.
    ///
    /// # Errors
    ///
    /// [`Error::Backend`] when the body is a failure other than not found, or
    /// is missing required fields.
    pub fn from_rpc(v: &Value) -> Result<Option<Self>> {
        if v.get("success").and_then(Value::as_bool) != Some(true) {
            let err = v.get("error").and_then(Value::as_str).unwrap_or("");
            if err.to_ascii_lowercase().contains("not found") {
                return Ok(None);
            }
            return Err(CatalogError::Backend(format!(
                "get_coin_record_by_name failed: {err}"
            )));
        }
        let r = v
            .get("coin_record")
            .ok_or_else(|| CatalogError::Backend("no coin_record".into()))?;
        if r.is_null() {
            return Ok(None);
        }
        let c = r
            .get("coin")
            .ok_or_else(|| CatalogError::Backend("no coin".into()))?;
        let s = |obj: &Value, k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .map(norm_hex)
                .ok_or_else(|| CatalogError::Backend(format!("no {k}")))
        };
        let n = |obj: &Value, k: &str| obj.get(k).and_then(Value::as_u64).unwrap_or(0);
        Ok(Some(Self {
            parent: s(c, "parent_coin_info")?,
            puzzle_hash: s(c, "puzzle_hash")?,
            amount: c
                .get("amount")
                .and_then(Value::as_u64)
                .ok_or_else(|| CatalogError::Backend("no amount".into()))?,
            spent: r.get("spent").and_then(Value::as_bool).unwrap_or(false)
                || n(r, "spent_block_index") > 0,
            spent_block_index: n(r, "spent_block_index"),
            confirmed_block_index: n(r, "confirmed_block_index"),
        }))
    }
}

/// Where the proven state sits on chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Build a small DataLayer style tree around the anchor leaf and return
/// a `get_proof` style JSON value plus the tree root hex. Used by tests
/// and by the Chia backend's unit tests.
pub fn fake_get_proof(
    entry: &SignedEntry,
    inner: &str,
    coin: &str,
    store: &str,
) -> Result<(Value, String)> {
    let leaf = datalayer_leaf_hash(FILE_HEAD_KEY.as_bytes(), &entry.to_bytes()?);
    let sib1 = datalayer_leaf_hash(b"ts/v1/catalog", b"x");
    let sib2 = tree_pair(
        &datalayer_leaf_hash(b"a", b"1"),
        &datalayer_leaf_hash(b"b", b"2"),
    );
    let l1 = tree_pair(&sib1, &leaf);
    let root = tree_pair(&l1, &sib2);
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
    Ok((resp, hex::encode(root)))
}

/// Check one full-node coin record against a verified proof (the same rules
/// [`crate`] clients apply after a successful HTTP lookup).
///
/// `peak_height` is required when the coin is spent (to measure the grace
/// window). It may be `None` when the coin is unspent.
///
/// # Errors
///
/// [`CatalogError::Chain`] when the record does not hash to the proof's coin
/// id, the puzzle hash does not match, or the amount is even.
/// [`CatalogError::Config`] when the coin is spent and `peak_height` is missing.
pub fn check_coin_record(
    proof: &VerifiedProof,
    rec: &CoinRecord,
    peak_height: Option<u64>,
    grace_blocks: u64,
) -> Result<CoinState> {
    let want_id = h32(&proof.coin_id, "coin_id")?;
    let parent = h32(&rec.parent, "parent_coin_info")?;
    let ph = h32(&rec.puzzle_hash, "puzzle_hash")?;
    if coin_id(&parent, &ph, rec.amount) != want_id {
        return Err(CatalogError::Chain(format!(
            "returned a coin record that does not hash to {}",
            proof.coin_id
        )));
    }
    if rec.puzzle_hash != proof.puzzle_hash {
        return Err(CatalogError::Chain(format!(
            "store coin {} has puzzle hash {}, but the proof needs {}              (the proof's root is not this store's root at that coin)",
            proof.coin_id, rec.puzzle_hash, proof.puzzle_hash
        )));
    }
    if rec.amount % 2 != 1 {
        return Err(CatalogError::Chain(format!(
            "coin {} has an even amount; not a singleton",
            proof.coin_id
        )));
    }
    if rec.spent {
        let peak = peak_height
            .ok_or_else(|| CatalogError::Config("spent coin record needs a peak height".into()))?;
        let blocks_ago = peak.saturating_sub(rec.spent_block_index);
        if blocks_ago <= grace_blocks {
            Ok(CoinState::RecentlySpent { blocks_ago })
        } else {
            Ok(CoinState::Stale { blocks_ago })
        }
    } else {
        Ok(CoinState::Current)
    }
}

/// Pick the more conservative of two coin states (stale wins).
pub fn worse_coin_state(a: &CoinState, b: CoinState) -> CoinState {
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
    use crate::Error as CatalogError;
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
        let (resp, root) = fake_get_proof(&entry, INNER1, COIN1, STORE).unwrap();
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
}
