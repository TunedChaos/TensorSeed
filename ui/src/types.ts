// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

export type VerifyMode = "strict" | "warn" | "off";

export type ThemePreference = "light" | "dark" | "system";

export type CatalogConfig = {
  name: string;
  displayName: string;
  baseUrl: string;
  pubkey: string;
  storeId: string;
  coinApiUrls: string[];
  mode: VerifyMode;
};

export type TrustAnchor = {
  revision: number;
  root: string;
};

export type HeadInfo = {
  status: "up_to_date" | "behind";
  target: number;
  revision: number;
  root: string;
  prev_root: string;
  expires_unix: number;
  snapshot: { path: string; revision: number; sha256: string } | null;
};

export type VerifiedProof = {
  store_id: string;
  root: string;
  coin_id: string;
  puzzle_hash: string;
  anchor: { catalog: string; revision: number; root: string; schema?: string };
};

export type CoinCheckJson = {
  state:
    | "Current"
    | { RecentlySpent: { blocks_ago: number } }
    | { Stale: { blocks_ago: number } };
  confirmed_block_index: number;
  spent: boolean;
  spent_block_index: number;
};

export type VerifyStatus =
  | { kind: "loading"; detail: string }
  | {
      kind: "verified";
      revision: number;
      root: string;
      expiresUnix: number;
      proof?: VerifiedProof;
      coin?: CoinCheckJson;
      warnings: string[];
    }
  | { kind: "unverified"; reason: string; warnings: string[]; revision?: number; root?: string };

export type IndexEntry = {
  schema: string;
  latest_commit: string;
  license_spdx: string | null;
  taken_down: boolean;
  total_bytes: number;
  updated_at: string;
};

export type ListedEntry = {
  id: string;
  repoType: "model" | "dataset";
  indexKey: string;
  latestKey: string;
  index: IndexEntry;
  /** Per entry signature result, when the source checks entries one by one (Desktop). */
  signature?: { ok: true } | { ok: false; error: string };
};

export type FileEntry = {
  path: string;
  size: number;
  sha256: string;
  format: string | null;
  pickle_risk: boolean;
};

export type Manifest = {
  schema: string;
  repo: { type: "model" | "dataset"; id: string; hf_url: string };
  hf_commit: string;
  license: {
    spdx: string | null;
    hf_tag: string | null;
    class: string;
    license_files: string[];
    attribution: string | null;
    gated_upstream: boolean;
  };
  totals: { files: number; bytes: number };
  files: FileEntry[];
  webseeds: string[];
  torrent: {
    infohash_v2: string;
    infohash_v1: string | null;
    magnet: string;
    name: string;
    piece_length: number;
    torrent_file_urls: string[];
  } | null;
};

export type CatalogAgreement = {
  id: string;
  same: boolean;
  left: string;
  right: string;
  leftCatalog: string;
  rightCatalog: string;
};
