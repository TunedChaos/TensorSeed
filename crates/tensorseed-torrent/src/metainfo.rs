// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Build and parse BEP 52 (v2) and hybrid v1+v2 `.torrent` files in pure Rust.
//!
//! Tensorseed torrents are always directory style: `name` is the content name
//! `{org}__{name}@{commit12}` and files keep their repo relative paths.
//! Output is deterministic (no creation date), so the same files, piece
//! length, trackers, and webseeds always give the same bytes and info hashes.
//! Hybrid layout matches libtorrent 2.x: files in canonical (path component)
//! order, each followed by a BEP 47 pad file `.pad/<n>` up to the next piece
//! boundary, including the last file. Info hashes were checked against
//! libtorrent-rasterbar 2.0.11 (see `docs/M0-TORRENT-SPIKE.md`).

use std::collections::BTreeMap;
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest, Sha256};
use tensorseed_core::manifest::{Manifest, TorrentInfo};
use tensorseed_core::verify::safe_join;

use crate::bencode::{self, Value};
use crate::merkle::{FileHasher, Hash32, hash_reader, root_padded, zero_piece_root};
use crate::{BuildParams, TorrentError};

/// Largest `.torrent` accepted by [`Metainfo::parse`].
pub const MAX_TORRENT_BYTES: usize = 64 << 20;

/// One input file for [`build_torrent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
    /// Path relative to the content directory, `/` separated.
    pub path: String,
    /// Expected length; the build fails if the file on disk differs.
    pub length: u64,
}

/// One file in a built or parsed torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    /// Path relative to the content directory, `/` separated.
    pub path: String,
    /// Length in bytes.
    pub length: u64,
    /// BEP 52 `pieces root`, `None` for empty files.
    pub pieces_root: Option<Hash32>,
    /// Plain SHA-256 of the file (known when built here, `None` when parsed).
    pub sha256: Option<Hash32>,
}

/// A parsed or freshly built torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metainfo {
    /// Encoded `.torrent` bytes.
    pub bytes: Vec<u8>,
    /// SHA-256 of the bencoded info dictionary.
    pub info_hash_v2: Hash32,
    /// SHA-1 of the info dictionary, for hybrid torrents.
    pub info_hash_v1: Option<[u8; 20]>,
    /// Torrent name.
    pub name: String,
    /// Piece length.
    pub piece_length: u64,
    /// Files in torrent order (pad files excluded).
    pub files: Vec<TorrentFile>,
    /// Piece layers by pieces root (files longer than one piece).
    pub piece_layers: BTreeMap<Hash32, Vec<Hash32>>,
    /// Tracker URLs (announce plus announce-list, deduplicated).
    pub trackers: Vec<String>,
    /// BEP 19 `url-list` entries.
    pub webseeds: Vec<String>,
}

fn components(path: &str) -> Result<Vec<&str>, TorrentError> {
    let c: Vec<&str> = path.split('/').collect();
    if path.is_empty() || c.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return Err(TorrentError::Invalid(format!("unsafe path {path:?}")));
    }
    Ok(c)
}

fn valid_piece_length(pl: u64) -> bool {
    pl >= crate::MIN_PIECE_LENGTH && pl.is_power_of_two() && pl <= (1 << 30)
}

/// Hash the files under `dir` and build a torrent.
///
/// `threads` bounds how many files are hashed at once (0 means the number
/// of CPUs). Files must exist with the given lengths.
pub fn build_torrent(
    dir: &Path,
    files: &[InputFile],
    params: &BuildParams,
    threads: usize,
) -> Result<Metainfo, TorrentError> {
    params.validate()?;
    if files.is_empty() {
        return Err(TorrentError::Invalid(
            "a torrent needs at least one file".into(),
        ));
    }
    let mut files: Vec<InputFile> = files.to_vec();
    for f in &files {
        components(&f.path)?;
    }
    files.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
    if files.windows(2).any(|w| w[0].path == w[1].path) {
        return Err(TorrentError::Invalid("duplicate file path".into()));
    }
    // A path may not be both a file and a directory.
    for w in files.windows(2) {
        if w[1].path.starts_with(&format!("{}/", w[0].path)) {
            return Err(TorrentError::Invalid(format!(
                "{} is both a file and a directory",
                w[0].path
            )));
        }
    }
    let pl = params.piece_length;
    let threads = if threads == 0 {
        std::thread::available_parallelism().map_or(4, |n| n.get())
    } else {
        threads
    }
    .min(files.len());

    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<Result<crate::merkle::FileHashes, TorrentError>>>> =
        files.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let r = (|| {
                        let p = safe_join(dir, &f.path)
                            .map_err(|e| TorrentError::Invalid(e.to_string()))?;
                        let fh = std::fs::File::open(&p)
                            .map_err(|e| TorrentError::Io(format!("{}: {e}", p.display())))?;
                        let len = fh
                            .metadata()
                            .map_err(|e| TorrentError::Io(e.to_string()))?
                            .len();
                        if len != f.length {
                            return Err(TorrentError::Invalid(format!(
                                "{}: length {len}, expected {}",
                                f.path, f.length
                            )));
                        }
                        let h = hash_reader(
                            std::io::BufReader::with_capacity(1 << 20, fh),
                            pl,
                            params.hybrid,
                        )
                        .map_err(|e| TorrentError::Io(format!("{}: {e}", p.display())))?;
                        if h.length != f.length {
                            return Err(TorrentError::Invalid(format!(
                                "{} changed while hashing",
                                f.path
                            )));
                        }
                        Ok(h)
                    })();
                    *results[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                }
            });
        }
    });
    let mut hashes = Vec::with_capacity(files.len());
    for r in results {
        hashes.push(
            r.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .expect("every file hashed")?,
        );
    }

    // Info dictionary.
    let mut tree: BTreeMap<Vec<u8>, Value> = BTreeMap::new();
    let mut v1_files = Vec::new();
    let mut v1_pieces = Vec::new();
    let mut layers: BTreeMap<Vec<u8>, Value> = BTreeMap::new();
    let mut out_files = Vec::with_capacity(files.len());
    for (f, h) in files.iter().zip(&hashes) {
        let mut leaf = BTreeMap::new();
        leaf.insert(b"length".to_vec(), Value::Int(f.length as i64));
        if let Some(r) = h.pieces_root {
            leaf.insert(b"pieces root".to_vec(), Value::bytes(r.to_vec()));
        }
        insert_tree(
            &mut tree,
            &components(&f.path)?,
            Value::dict([("", Value::Dict(leaf))]),
        )?;
        if !h.piece_layer.is_empty() {
            let root = h.pieces_root.expect("non empty");
            layers.insert(root.to_vec(), Value::bytes(h.piece_layer.concat()));
        }
        if params.hybrid {
            v1_files.push(Value::dict([
                ("length", Value::Int(f.length as i64)),
                (
                    "path",
                    Value::List(f.path.split('/').map(Value::bytes).collect()),
                ),
            ]));
            let rem = f.length % pl;
            if rem != 0 {
                let pad = pl - rem;
                v1_files.push(Value::dict([
                    ("attr", Value::bytes("p")),
                    ("length", Value::Int(pad as i64)),
                    (
                        "path",
                        Value::List(vec![Value::bytes(".pad"), Value::bytes(pad.to_string())]),
                    ),
                ]));
            }
            for p in &h.v1_pieces {
                v1_pieces.extend_from_slice(p);
            }
        }
        out_files.push(TorrentFile {
            path: f.path.clone(),
            length: f.length,
            pieces_root: h.pieces_root,
            sha256: Some(h.sha256),
        });
    }
    let mut info = BTreeMap::new();
    info.insert(b"file tree".to_vec(), Value::Dict(tree));
    info.insert(b"meta version".to_vec(), Value::Int(2));
    info.insert(b"name".to_vec(), Value::bytes(params.name.clone()));
    info.insert(b"piece length".to_vec(), Value::Int(pl as i64));
    if params.hybrid {
        info.insert(b"files".to_vec(), Value::List(v1_files));
        info.insert(b"pieces".to_vec(), Value::bytes(v1_pieces));
    }
    let info = Value::Dict(info);

    let mut top = BTreeMap::new();
    if let Some(first) = params.trackers.first() {
        top.insert(b"announce".to_vec(), Value::bytes(first.clone()));
        if params.trackers.len() > 1 {
            top.insert(
                b"announce-list".to_vec(),
                Value::List(
                    params
                        .trackers
                        .iter()
                        .map(|t| Value::List(vec![Value::bytes(t.clone())]))
                        .collect(),
                ),
            );
        }
    }
    top.insert(
        b"created by".to_vec(),
        Value::bytes(concat!("tensorseed/", env!("CARGO_PKG_VERSION"))),
    );
    top.insert(b"info".to_vec(), info);
    if !layers.is_empty() {
        top.insert(b"piece layers".to_vec(), Value::Dict(layers));
    }
    if !params.webseeds.is_empty() {
        top.insert(
            b"url-list".to_vec(),
            Value::List(
                params
                    .webseeds
                    .iter()
                    .map(|w| Value::bytes(w.clone()))
                    .collect(),
            ),
        );
    }
    let bytes = Value::Dict(top).encode();
    let mut m = Metainfo::parse(&bytes)?;
    for (a, b) in m.files.iter_mut().zip(out_files) {
        debug_assert_eq!(a.path, b.path);
        a.sha256 = b.sha256;
    }
    Ok(m)
}

fn insert_tree(
    tree: &mut BTreeMap<Vec<u8>, Value>,
    comps: &[&str],
    leaf: Value,
) -> Result<(), TorrentError> {
    let (first, rest) = comps.split_first().expect("non empty");
    if rest.is_empty() {
        tree.insert(first.as_bytes().to_vec(), leaf);
        return Ok(());
    }
    let child = tree
        .entry(first.as_bytes().to_vec())
        .or_insert_with(|| Value::Dict(BTreeMap::new()));
    match child {
        Value::Dict(d) => insert_tree(d, rest, leaf),
        _ => Err(TorrentError::Invalid("file tree conflict".into())),
    }
}

fn bad(msg: impl Into<String>) -> TorrentError {
    TorrentError::Invalid(msg.into())
}

impl Metainfo {
    /// Parse and validate a v2 or hybrid torrent. v1 only torrents are
    /// rejected: Tensorseed needs the per file Merkle roots.
    pub fn parse(bytes: &[u8]) -> Result<Self, TorrentError> {
        if bytes.len() > MAX_TORRENT_BYTES {
            return Err(bad("torrent file too large"));
        }
        let (top, info_raw) =
            bencode::decode_with_span(bytes, "info").map_err(|e| bad(e.to_string()))?;
        let info_raw = info_raw.ok_or_else(|| bad("missing info"))?;
        let info = top.get("info").expect("span implies key");
        if info.get("meta version").and_then(Value::as_int) != Some(2) {
            return Err(bad("not a BitTorrent v2 torrent (meta version != 2)"));
        }
        let name = info
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty() && !n.contains('/') && *n != "." && *n != "..")
            .ok_or_else(|| bad("bad name"))?
            .to_owned();
        let pl = info
            .get("piece length")
            .and_then(Value::as_int)
            .filter(|&p| p > 0 && valid_piece_length(p as u64))
            .ok_or_else(|| bad("bad piece length"))? as u64;
        let tree = info
            .get("file tree")
            .and_then(Value::as_dict)
            .ok_or_else(|| bad("missing file tree"))?;
        let mut files = Vec::new();
        walk_tree(tree, &mut Vec::new(), &mut files, 0)?;
        if files.is_empty() {
            return Err(bad("empty file tree"));
        }

        // Piece layers.
        let mut piece_layers = BTreeMap::new();
        let layers = top.get("piece layers").and_then(Value::as_dict);
        let zero = zero_piece_root(pl as usize);
        for f in &files {
            if f.length <= pl {
                continue;
            }
            let root = f.pieces_root.expect("checked in walk");
            let raw = layers
                .and_then(|l| l.get(root.as_slice()))
                .and_then(Value::as_bytes)
                .ok_or_else(|| bad(format!("missing piece layer for {}", f.path)))?;
            let n = f.length.div_ceil(pl) as usize;
            if raw.len() != n * 32 {
                return Err(bad(format!("piece layer for {} has wrong size", f.path)));
            }
            let layer: Vec<Hash32> = raw
                .chunks(32)
                .map(|c| c.try_into().expect("32 bytes"))
                .collect();
            if root_padded(&layer, n.next_power_of_two(), zero) != root {
                return Err(bad(format!(
                    "piece layer for {} does not match its root",
                    f.path
                )));
            }
            piece_layers.insert(root, layer);
        }

        // Hybrid: the v1 view must describe the same files, piece aligned.
        let info_hash_v1 = if let Some(pieces) = info.get("pieces") {
            let pieces = pieces.as_bytes().ok_or_else(|| bad("bad pieces"))?;
            let list = info
                .get("files")
                .and_then(Value::as_list)
                .ok_or_else(|| bad("hybrid torrent without a v1 files list"))?;
            let mut real = Vec::new();
            let mut total: u64 = 0;
            for e in list {
                let len = e
                    .get("length")
                    .and_then(Value::as_int)
                    .filter(|&l| l >= 0)
                    .ok_or_else(|| bad("bad v1 file length"))? as u64;
                total = total.checked_add(len).ok_or_else(|| bad("v1 too large"))?;
                let attr = e.get("attr").and_then(Value::as_str).unwrap_or("");
                if attr.contains('p') {
                    continue;
                }
                let path: Vec<&str> = e
                    .get("path")
                    .and_then(Value::as_list)
                    .ok_or_else(|| bad("bad v1 path"))?
                    .iter()
                    .map(|c| c.as_str().ok_or_else(|| bad("bad v1 path")))
                    .collect::<Result<_, _>>()?;
                real.push((path.join("/"), len));
            }
            let v2: Vec<(String, u64)> = files.iter().map(|f| (f.path.clone(), f.length)).collect();
            if real != v2 {
                return Err(bad("v1 and v2 file lists differ"));
            }
            if pieces.len() as u64 != total.div_ceil(pl) * 20 {
                return Err(bad("v1 pieces length does not match the files"));
            }
            Some(Sha1::digest(info_raw).into())
        } else {
            None
        };

        let mut trackers: Vec<String> = Vec::new();
        if let Some(a) = top.get("announce").and_then(Value::as_str) {
            trackers.push(a.to_owned());
        }
        for tier in top
            .get("announce-list")
            .and_then(Value::as_list)
            .unwrap_or(&[])
        {
            for t in tier.as_list().unwrap_or(&[]) {
                if let Some(t) = t.as_str()
                    && !trackers.iter().any(|x| x == t)
                {
                    trackers.push(t.to_owned());
                }
            }
        }
        let webseeds = match top.get("url-list") {
            Some(Value::Bytes(_)) => vec![
                top.get("url-list")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            ],
            Some(Value::List(l)) => l
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect();

        Ok(Self {
            bytes: bytes.to_vec(),
            info_hash_v2: Sha256::digest(info_raw).into(),
            info_hash_v1,
            name,
            piece_length: pl,
            files,
            piece_layers,
            trackers,
            webseeds,
        })
    }

    /// Lowercase hex v2 info hash.
    pub fn info_hash_v2_hex(&self) -> String {
        hex::encode(self.info_hash_v2)
    }

    /// Lowercase hex v1 info hash, for hybrid torrents.
    pub fn info_hash_v1_hex(&self) -> Option<String> {
        self.info_hash_v1.map(hex::encode)
    }

    /// Magnet link: `btih` (hybrid only) and `btmh`, name, trackers, webseeds.
    pub fn magnet(&self) -> String {
        let mut m = String::from("magnet:?");
        if let Some(v1) = self.info_hash_v1_hex() {
            m.push_str(&format!("xt=urn:btih:{v1}&"));
        }
        m.push_str(&format!("xt=urn:btmh:1220{}", self.info_hash_v2_hex()));
        m.push_str(&format!("&dn={}", pct(&self.name)));
        for t in &self.trackers {
            m.push_str(&format!("&tr={}", pct(t)));
        }
        for w in &self.webseeds {
            m.push_str(&format!("&ws={}", pct(w)));
        }
        m
    }

    /// Manifest `torrent` block for this torrent.
    pub fn torrent_info(&self, torrent_file_urls: Vec<String>) -> TorrentInfo {
        TorrentInfo {
            infohash_v2: self.info_hash_v2_hex(),
            infohash_v1: self.info_hash_v1_hex(),
            magnet: self.magnet(),
            name: self.name.clone(),
            piece_length: self.piece_length,
            torrent_file_urls,
        }
    }

    /// File by path.
    pub fn file(&self, path: &str) -> Option<&TorrentFile> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Check that this torrent describes exactly the manifest's files: same
    /// name, paths, and sizes; Merkle roots and info hashes equal wherever the
    /// manifest records them. Paged manifests must pass their files in
    /// `files`.
    pub fn check_manifest(
        &self,
        m: &Manifest,
        files: &[tensorseed_core::manifest::FileEntry],
    ) -> Result<(), TorrentError> {
        if self.name != m.content_name() {
            return Err(bad(format!(
                "torrent name {:?} does not match {:?}",
                self.name,
                m.content_name()
            )));
        }
        if let Some(t) = &m.torrent {
            if t.infohash_v2 != self.info_hash_v2_hex() {
                return Err(bad("torrent v2 info hash does not match the manifest"));
            }
            if t.infohash_v1.is_some() && t.infohash_v1 != self.info_hash_v1_hex() {
                return Err(bad("torrent v1 info hash does not match the manifest"));
            }
            if t.piece_length != self.piece_length {
                return Err(bad("torrent piece length does not match the manifest"));
            }
        }
        if files.len() != self.files.len() {
            return Err(bad(format!(
                "torrent has {} files, manifest has {}",
                self.files.len(),
                files.len()
            )));
        }
        for e in files {
            let f = self
                .file(&e.path)
                .ok_or_else(|| bad(format!("{} is not in the torrent", e.path)))?;
            if f.length != e.size {
                return Err(bad(format!("{}: size differs from the manifest", e.path)));
            }
            if let Some(r) = &e.bt_v2_pieces_root
                && f.pieces_root.map(hex::encode).as_deref() != Some(r.as_str())
            {
                return Err(bad(format!(
                    "{}: pieces root differs from the manifest",
                    e.path
                )));
            }
        }
        Ok(())
    }

    /// Re-hash a file on disk and compare it with this torrent's Merkle root.
    pub fn verify_file(&self, path: &str, on_disk: &Path) -> Result<bool, TorrentError> {
        let f = self
            .file(path)
            .ok_or_else(|| bad(format!("{path} is not in the torrent")))?;
        let fh = std::fs::File::open(on_disk)
            .map_err(|e| TorrentError::Io(format!("{}: {e}", on_disk.display())))?;
        let mut h = FileHasher::new(self.piece_length, false);
        let mut r = std::io::BufReader::with_capacity(1 << 20, fh);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = std::io::Read::read(&mut r, &mut buf)
                .map_err(|e| TorrentError::Io(e.to_string()))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        let h = h.finish();
        Ok(h.length == f.length && h.pieces_root == f.pieces_root)
    }
}

fn walk_tree(
    d: &BTreeMap<Vec<u8>, Value>,
    prefix: &mut Vec<String>,
    out: &mut Vec<TorrentFile>,
    depth: usize,
) -> Result<(), TorrentError> {
    if depth > 32 {
        return Err(bad("file tree too deep"));
    }
    for (k, v) in d {
        let k = std::str::from_utf8(k).map_err(|_| bad("non UTF-8 path"))?;
        if k.is_empty() || k == "." || k == ".." || k.contains('/') || k.contains('\\') {
            return Err(bad(format!("unsafe path component {k:?}")));
        }
        let v = v.as_dict().ok_or_else(|| bad("bad file tree node"))?;
        prefix.push(k.to_owned());
        if let Some(leaf) = v.get(b"".as_slice()) {
            if v.len() != 1 {
                return Err(bad("file tree node is both file and directory"));
            }
            let length = leaf
                .get("length")
                .and_then(Value::as_int)
                .filter(|&l| l >= 0)
                .ok_or_else(|| bad("bad file length"))? as u64;
            let pieces_root = match leaf.get("pieces root") {
                Some(r) => Some(
                    <Hash32>::try_from(r.as_bytes().ok_or_else(|| bad("bad pieces root"))?)
                        .map_err(|_| bad("bad pieces root"))?,
                ),
                None => None,
            };
            if (length > 0) != pieces_root.is_some() {
                return Err(bad(
                    "pieces root must be present exactly for non empty files",
                ));
            }
            out.push(TorrentFile {
                path: prefix.join("/"),
                length,
                pieces_root,
                sha256: None,
            });
        } else {
            walk_tree(v, prefix, out, depth + 1)?;
        }
        prefix.pop();
    }
    Ok(())
}

/// Percent encode everything except RFC 3986 unreserved characters.
pub fn pct(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deterministic fixture; the same generator in Python fed libtorrent
    /// 2.0.11 to produce the reference hashes below.
    pub(crate) const FIXTURE: &[(&str, usize)] = &[
        ("empty.txt", 0),
        ("a/empty2", 0),
        ("a.txt", 2),
        ("a/b/exact16k", 16384),
        ("a/b/exact64k", 65536),
        ("z/odd.bin", 100_001),
        ("z/big.bin", 300_000),
        ("z/\u{fc}n\u{ef} code.json", 5),
        ("Upper.md", 1),
    ];
    pub(crate) const NAME: &str = "org__fx@aaaaaaaaaaaa";

    pub(crate) fn content(i: usize, n: usize) -> Vec<u8> {
        let mut x: u64 = i as u64 + 1;
        (0..n)
            .map(|_| {
                x = x
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (x >> 56) as u8
            })
            .collect()
    }

    pub(crate) fn write_fixture(dir: &Path) -> Vec<InputFile> {
        FIXTURE
            .iter()
            .enumerate()
            .map(|(i, (p, n))| {
                let fp = dir.join(p);
                std::fs::create_dir_all(fp.parent().unwrap()).unwrap();
                std::fs::write(&fp, content(i, *n)).unwrap();
                InputFile {
                    path: (*p).into(),
                    length: *n as u64,
                }
            })
            .collect()
    }

    fn params(pl: u64, hybrid: bool) -> BuildParams {
        BuildParams {
            name: NAME.into(),
            piece_length: pl,
            hybrid,
            trackers: vec![],
            webseeds: vec!["http://127.0.0.1:1/t/".into()],
        }
    }

    #[test]
    fn info_hashes_match_libtorrent() {
        let d = tempfile::tempdir().unwrap();
        let files = write_fixture(d.path());
        let cases = [
            (
                16384,
                false,
                None,
                "a32567140639e43d8c2510e26c9ff17f9275dd61c0b72322769e034f67623aaf",
            ),
            (
                16384,
                true,
                Some("a190ed05501bf477b01ac1b0b0be7fb76d15780e"),
                "74f7b9e53c956637af56f8ac1457269d311c9cd835342eea7d8d42f6de98b919",
            ),
            (
                65536,
                false,
                None,
                "d6f5d374d089b8166f95a0d63d30dc3524571a48f2fb9058c2ee4c74740f3d91",
            ),
            (
                65536,
                true,
                Some("6969a40c4e06b29afcc33acc53476345b6f3b609"),
                "30b0b71f5441d49df5cef597a6d98e0ed9f63f7d7f7a47f7a8166cc590fd91ca",
            ),
        ];
        for (pl, hybrid, v1, v2) in cases {
            let m = build_torrent(d.path(), &files, &params(pl, hybrid), 2).unwrap();
            assert_eq!(m.info_hash_v2_hex(), v2, "pl={pl} hybrid={hybrid}");
            assert_eq!(
                m.info_hash_v1_hex().as_deref(),
                v1,
                "pl={pl} hybrid={hybrid}"
            );
            // Deterministic, and the parse of our own bytes is identical.
            let again = build_torrent(d.path(), &files, &params(pl, hybrid), 1).unwrap();
            assert_eq!(again.bytes, m.bytes);
            let mut parsed = Metainfo::parse(&m.bytes).unwrap();
            for f in &mut parsed.files {
                f.sha256 = m.file(&f.path).unwrap().sha256;
            }
            assert_eq!(parsed, m);
        }
    }

    #[test]
    fn metadata_and_magnet() {
        let d = tempfile::tempdir().unwrap();
        let files = write_fixture(d.path());
        let mut p = params(16384, true);
        p.trackers = vec![
            "udp://t.example:1337/announce".into(),
            "https://t2.example/a".into(),
        ];
        let m = build_torrent(d.path(), &files, &p, 0).unwrap();
        assert_eq!(m.files.len(), FIXTURE.len());
        assert_eq!(m.files[0].path, "Upper.md");
        assert_eq!(m.trackers, p.trackers);
        assert_eq!(m.webseeds, p.webseeds);
        // big.bin (300000 bytes) spans 19 pieces of 16 KiB.
        let big = m.file("z/big.bin").unwrap();
        assert_eq!(m.piece_layers[&big.pieces_root.unwrap()].len(), 19);
        assert!(m.file("empty.txt").unwrap().pieces_root.is_none());
        let mag = m.magnet();
        assert!(mag.starts_with(&format!(
            "magnet:?xt=urn:btih:{}&xt=urn:btmh:1220{}&dn=org__fx%40aaaaaaaaaaaa&tr=udp%3A%2F%2F",
            m.info_hash_v1_hex().unwrap(),
            m.info_hash_v2_hex()
        )));
        assert!(mag.ends_with("&ws=http%3A%2F%2F127.0.0.1%3A1%2Ft%2F"));
        let ti = m.torrent_info(vec![]);
        assert_eq!(ti.name, NAME);
        assert_eq!(ti.piece_length, 16384);
    }

    #[test]
    fn verify_file_detects_corruption() {
        let d = tempfile::tempdir().unwrap();
        let files = write_fixture(d.path());
        let m = build_torrent(d.path(), &files, &params(16384, true), 0).unwrap();
        let p = d.path().join("z/big.bin");
        assert!(m.verify_file("z/big.bin", &p).unwrap());
        let mut b = std::fs::read(&p).unwrap();
        b[200_000] ^= 1;
        std::fs::write(&p, &b).unwrap();
        assert!(!m.verify_file("z/big.bin", &p).unwrap());
        assert!(m.verify_file("missing", &p).is_err());
    }

    #[test]
    fn build_rejects_bad_input() {
        let d = tempfile::tempdir().unwrap();
        let mut files = write_fixture(d.path());
        assert!(build_torrent(d.path(), &[], &params(16384, true), 0).is_err());
        files[2].length += 1;
        assert!(build_torrent(d.path(), &files, &params(16384, true), 0).is_err());
        files[2].length -= 1;
        let mut dup = files.clone();
        dup.push(files[0].clone());
        assert!(build_torrent(d.path(), &dup, &params(16384, true), 0).is_err());
        let mut esc = files.clone();
        esc[0].path = "../x".into();
        assert!(build_torrent(d.path(), &esc, &params(16384, true), 0).is_err());
        assert!(build_torrent(d.path(), &files, &params(1000, true), 0).is_err());
    }

    fn with_info(bytes: &[u8], f: impl FnOnce(&mut BTreeMap<Vec<u8>, Value>)) -> Vec<u8> {
        let mut top = bencode::decode(bytes).unwrap();
        let Value::Dict(t) = &mut top else { panic!() };
        let Some(Value::Dict(info)) = t.get_mut(b"info".as_slice()) else {
            panic!()
        };
        f(info);
        top.encode()
    }

    #[test]
    fn parse_rejects_tampering() {
        let d = tempfile::tempdir().unwrap();
        let files = write_fixture(d.path());
        let m = build_torrent(d.path(), &files, &params(16384, true), 0).unwrap();
        // v1 only torrent.
        let v1 = with_info(&m.bytes, |i| {
            i.remove(b"meta version".as_slice());
            i.remove(b"file tree".as_slice());
        });
        assert!(Metainfo::parse(&v1).unwrap_err().to_string().contains("v2"));
        // Wrong piece layer.
        let mut top = bencode::decode(&m.bytes).unwrap();
        if let Value::Dict(t) = &mut top
            && let Some(Value::Dict(l)) = t.get_mut(b"piece layers".as_slice())
        {
            for v in l.values_mut() {
                if let Value::Bytes(b) = v {
                    b[0] ^= 1;
                }
            }
        }
        assert!(Metainfo::parse(&top.encode()).is_err());
        // v1 list that disagrees with the file tree.
        let bad_v1 = with_info(&m.bytes, |i| {
            if let Some(Value::List(l)) = i.get_mut(b"files".as_slice()) {
                l.swap(0, 2);
            }
        });
        assert!(Metainfo::parse(&bad_v1).is_err());
        // Path traversal in the file tree.
        let trav = with_info(&m.bytes, |i| {
            if let Some(Value::Dict(t)) = i.get_mut(b"file tree".as_slice()) {
                let v = t.remove(b"a.txt".as_slice()).unwrap();
                t.insert(b"..".to_vec(), v);
            }
        });
        assert!(Metainfo::parse(&trav).is_err());
        assert!(Metainfo::parse(b"de").is_err());
    }
}
