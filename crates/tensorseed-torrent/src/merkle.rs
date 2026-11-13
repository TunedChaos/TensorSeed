// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! BEP 52 per file Merkle trees, plus the aligned v1 piece hashes a hybrid
//! torrent needs, plus the plain SHA-256 of the file, all in one pass.
//!
//! Rules (<https://www.bittorrent.org/beps/bep_0052.html>):
//! - Leaves are SHA-256 of each 16 KiB block; the last block of a file may be
//!   short and is hashed as is.
//! - The leaf layer is padded with zero hashes to a power of two. For a file
//!   larger than one piece that is the same as padding every piece to
//!   `piece_length / 16 KiB` leaves and the piece layer to a power of two with
//!   the root of an all zero piece.
//! - `pieces root` is the tree root. Files larger than one piece also publish
//!   their piece layer (`piece layers` in the torrent).
//! - In a hybrid torrent each file starts on a piece boundary (BEP 47 pad
//!   files), so v1 pieces never span files and the last v1 piece of a file is
//!   hashed with zero padding up to the piece length.

use sha1::Sha1;
use sha2::{Digest, Sha256};

/// BEP 52 block size.
pub const BLOCK: usize = 16 * 1024;

/// A SHA-256 node.
pub type Hash32 = [u8; 32];

/// Output for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHashes {
    /// File length in bytes.
    pub length: u64,
    /// Plain SHA-256 of the whole file.
    pub sha256: Hash32,
    /// BEP 52 `pieces root`; `None` for an empty file.
    pub pieces_root: Option<Hash32>,
    /// Piece layer (one hash per piece); only for files longer than one piece.
    pub piece_layer: Vec<Hash32>,
    /// v1 SHA-1 piece hashes (hybrid torrents), zero padded at the end.
    pub v1_pieces: Vec<[u8; 20]>,
}

/// Streaming hasher for one file.
pub struct FileHasher {
    piece_length: usize,
    v1: bool,
    length: u64,
    whole: Sha256,
    block: Vec<u8>,
    piece_leaves: Vec<Hash32>,
    piece_layer: Vec<Hash32>,
    v1_hasher: Sha1,
    v1_fill: usize,
    v1_pieces: Vec<[u8; 20]>,
}

/// Merkle root of `leaves` padded with `pad` to `width` (a power of two).
pub fn root_padded(leaves: &[Hash32], width: usize, pad: Hash32) -> Hash32 {
    debug_assert!(width.is_power_of_two() && leaves.len() <= width);
    let mut layer: Vec<Hash32> = leaves.to_vec();
    layer.resize(width, pad);
    let mut pad = pad;
    while layer.len() > 1 {
        layer = layer
            .chunks(2)
            .map(|p| {
                let mut h = Sha256::new();
                h.update(p[0]);
                h.update(p[1]);
                h.finalize().into()
            })
            .collect();
        let mut h = Sha256::new();
        h.update(pad);
        h.update(pad);
        pad = h.finalize().into();
    }
    layer[0]
}

/// Root of a piece made of zero leaves (the pad for piece layers).
pub fn zero_piece_root(piece_length: usize) -> Hash32 {
    root_padded(&[], piece_length / BLOCK, [0; 32])
}

impl FileHasher {
    /// New hasher. `piece_length` must be a power of two and at least 16 KiB.
    /// With `v1`, also compute aligned v1 piece hashes.
    ///
    /// # Panics
    ///
    /// If `piece_length` is not a power of two or is smaller than 16 KiB.
    pub fn new(piece_length: u64, v1: bool) -> Self {
        let piece_length = piece_length as usize;
        assert!(piece_length >= BLOCK && piece_length.is_power_of_two());
        Self {
            piece_length,
            v1,
            length: 0,
            whole: Sha256::new(),
            block: Vec::with_capacity(BLOCK),
            piece_leaves: Vec::with_capacity(piece_length / BLOCK),
            piece_layer: Vec::new(),
            v1_hasher: Sha1::new(),
            v1_fill: 0,
            v1_pieces: Vec::new(),
        }
    }

    /// Feed bytes.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;
        self.whole.update(data);
        if self.v1 {
            let mut d = data;
            while !d.is_empty() {
                let n = (self.piece_length - self.v1_fill).min(d.len());
                self.v1_hasher.update(&d[..n]);
                self.v1_fill += n;
                d = &d[n..];
                if self.v1_fill == self.piece_length {
                    self.v1_pieces
                        .push(std::mem::take(&mut self.v1_hasher).finalize().into());
                    self.v1_fill = 0;
                }
            }
        }
        while !data.is_empty() {
            let n = (BLOCK - self.block.len()).min(data.len());
            self.block.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.block.len() == BLOCK {
                self.push_leaf();
            }
        }
    }

    fn push_leaf(&mut self) {
        let leaf: Hash32 = Sha256::digest(&self.block).into();
        self.block.clear();
        self.piece_leaves.push(leaf);
        if self.piece_leaves.len() == self.piece_length / BLOCK {
            self.close_piece();
        }
    }

    fn close_piece(&mut self) {
        let root = root_padded(&self.piece_leaves, self.piece_length / BLOCK, [0; 32]);
        self.piece_layer.push(root);
        self.piece_leaves.clear();
    }

    /// Finish and return the hashes.
    pub fn finish(mut self) -> FileHashes {
        if !self.block.is_empty() {
            self.push_leaf();
        }
        let length = self.length;
        let mut piece_layer = Vec::new();
        let pieces_root = if length == 0 {
            None
        } else if length <= self.piece_length as u64 {
            // One piece or less: the tree covers just this file's leaves
            // (an exactly full piece was already closed into the layer).
            Some(match self.piece_layer.first() {
                Some(r) => *r,
                None => root_padded(
                    &self.piece_leaves,
                    self.piece_leaves.len().next_power_of_two(),
                    [0; 32],
                ),
            })
        } else {
            if !self.piece_leaves.is_empty() {
                self.close_piece();
            }
            piece_layer = std::mem::take(&mut self.piece_layer);
            let width = piece_layer.len().next_power_of_two();
            Some(root_padded(
                &piece_layer,
                width,
                zero_piece_root(self.piece_length),
            ))
        };
        let mut v1_pieces = std::mem::take(&mut self.v1_pieces);
        if self.v1 && self.v1_fill > 0 {
            let zeros = vec![0u8; self.piece_length - self.v1_fill];
            self.v1_hasher.update(&zeros);
            v1_pieces.push(self.v1_hasher.finalize().into());
        }
        FileHashes {
            length,
            sha256: self.whole.finalize().into(),
            pieces_root,
            piece_layer,
            v1_pieces,
        }
    }
}

/// Hash a whole reader.
///
/// # Errors
///
/// Any I/O error from `r`.
pub fn hash_reader<R: std::io::Read>(
    mut r: R,
    piece_length: u64,
    v1: bool,
) -> std::io::Result<FileHashes> {
    let mut h = FileHasher::new(piece_length, v1);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(a: &[u8]) -> Hash32 {
        Sha256::digest(a).into()
    }
    fn pair(a: Hash32, b: Hash32) -> Hash32 {
        let mut h = Sha256::new();
        h.update(a);
        h.update(b);
        h.finalize().into()
    }

    #[test]
    fn small_file_root_is_padded_leaf_tree() {
        // 3 blocks (last short) in a 64 KiB piece: leaves padded to 4.
        let data: Vec<u8> = (0..(2 * BLOCK + 100)).map(|i| (i % 251) as u8).collect();
        let h = hash_reader(&data[..], 64 * 1024, true).unwrap();
        let l: Vec<Hash32> = data.chunks(BLOCK).map(sha).collect();
        let want = pair(pair(l[0], l[1]), pair(l[2], [0; 32]));
        assert_eq!(h.pieces_root, Some(want));
        assert!(h.piece_layer.is_empty());
        let mut padded = data.clone();
        padded.resize(64 * 1024, 0);
        assert_eq!(h.v1_pieces, vec![<[u8; 20]>::from(Sha1::digest(&padded))]);
        assert_eq!(h.sha256, sha(&data));
    }

    #[test]
    fn single_block_file_root_is_leaf() {
        let h = hash_reader(&b"hello"[..], 16 * 1024, false).unwrap();
        assert_eq!(h.pieces_root, Some(sha(b"hello")));
        assert!(h.v1_pieces.is_empty());
    }

    #[test]
    fn multi_piece_file_uses_piece_layer() {
        // piece = 2 blocks; 5 blocks -> 3 pieces -> layer padded to 4.
        let pl = 2 * BLOCK;
        let data: Vec<u8> = (0..(5 * BLOCK)).map(|i| (i * 7 % 256) as u8).collect();
        let h = hash_reader(&data[..], pl as u64, false).unwrap();
        let l: Vec<Hash32> = data.chunks(BLOCK).map(sha).collect();
        let p = [pair(l[0], l[1]), pair(l[2], l[3]), pair(l[4], [0; 32])];
        assert_eq!(h.piece_layer, p.to_vec());
        let zp = pair([0; 32], [0; 32]);
        assert_eq!(h.pieces_root, Some(pair(pair(p[0], p[1]), pair(p[2], zp))));
        // Same as padding the leaf layer to 8 zero leaves.
        let mut leaves = l.clone();
        leaves.resize(8, [0; 32]);
        assert_eq!(h.pieces_root, Some(root_padded(&leaves, 8, [0; 32])));
    }

    #[test]
    fn exact_one_piece_and_empty() {
        let data = vec![9u8; 2 * BLOCK];
        let h = hash_reader(&data[..], 2 * BLOCK as u64, true).unwrap();
        assert_eq!(
            h.pieces_root,
            Some(pair(sha(&data[..BLOCK]), sha(&data[BLOCK..])))
        );
        assert!(h.piece_layer.is_empty());
        assert_eq!(h.v1_pieces.len(), 1);
        let e = hash_reader(&b""[..], 2 * BLOCK as u64, true).unwrap();
        assert_eq!(e.pieces_root, None);
        assert!(e.v1_pieces.is_empty());
    }
}
