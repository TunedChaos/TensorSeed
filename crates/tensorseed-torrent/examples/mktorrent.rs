// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Build a Tensorseed torrent for a directory (M0 spike and cross checks).
//!
//! Usage: mktorrent <content dir> <out.torrent> <piece length> <hybrid|v2> [webseed base]...
//! The torrent name is the directory's file name. Prints the info hashes.

use tensorseed_torrent::{BuildParams, InputFile, build_torrent};

fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<InputFile>) {
    for e in std::fs::read_dir(dir).expect("read_dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            let rel = p.strip_prefix(root).expect("prefix");
            out.push(InputFile {
                path: rel.to_str().expect("utf8").replace('\\', "/"),
                length: std::fs::metadata(&p).expect("meta").len(),
            });
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let mut files = Vec::new();
    walk(&dir, &dir, &mut files);
    let params = BuildParams {
        name: dir
            .file_name()
            .expect("name")
            .to_str()
            .expect("utf8")
            .into(),
        piece_length: a[3].parse().expect("piece length"),
        hybrid: a[4] == "hybrid",
        trackers: vec![],
        webseeds: a[5..].to_vec(),
    };
    let t0 = std::time::Instant::now();
    let m = build_torrent(&dir, &files, &params, 0).expect("build");
    std::fs::write(&a[2], &m.bytes).expect("write");
    println!(
        "v1={}\tv2={}\tfiles={}\ttime={:?}\tmagnet={}",
        m.info_hash_v1_hex().unwrap_or_else(|| "-".into()),
        m.info_hash_v2_hex(),
        m.files.len(),
        t0.elapsed(),
        m.magnet()
    );
}
