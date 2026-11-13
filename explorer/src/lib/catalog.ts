// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import { CatalogVerifier, check_coin_rpc, initWasm, wasm_parse_manifest } from "./wasm";
import { catalogDir, joinUrl } from "./format";
import { DEFAULT_GRACE_BLOCKS, MAX_ARTIFACT_BYTES } from "./official";
import { loadAnchors, saveAnchor } from "./storage";
import type {
  CatalogAgreement,
  CatalogConfig,
  CoinCheckJson,
  HeadInfo,
  IndexEntry,
  ListedEntry,
  Manifest,
  TrustAnchor,
  VerifiedProof,
  VerifyStatus,
} from "./types";

export type LoadedCatalog = {
  config: CatalogConfig;
  verifier: CatalogVerifier;
  status: VerifyStatus;
  entries: ListedEntry[];
  info: { display_name?: string; contact?: string; policy_url?: string; name?: string } | null;
};

async function fetchBytes(url: string): Promise<Uint8Array> {
  const res = await fetch(url, { mode: "cors", credentials: "omit", redirect: "follow" });
  if (!res.ok) {
    throw new Error(`${url}: HTTP ${res.status}`);
  }
  const len = Number(res.headers.get("content-length") || "0");
  if (len > MAX_ARTIFACT_BYTES) {
    throw new Error(`${url}: larger than ${MAX_ARTIFACT_BYTES} bytes`);
  }
  const buf = new Uint8Array(await res.arrayBuffer());
  if (buf.byteLength > MAX_ARTIFACT_BYTES) {
    throw new Error(`${url}: larger than ${MAX_ARTIFACT_BYTES} bytes`);
  }
  return buf;
}

async function fetchText(url: string): Promise<string> {
  return new TextDecoder().decode(await fetchBytes(url));
}

function deltaPath(rev: number): string {
  return `deltas/${String(rev).padStart(10, "0")}.json`;
}

function snapshotPath(rev: number): string {
  return `snapshots/${String(rev).padStart(10, "0")}.json`;
}

async function fetchSigned(dir: string, rel: string): Promise<{ body: Uint8Array; sig: string }> {
  const body = await fetchBytes(joinUrl(dir, rel));
  const sig = await fetchText(joinUrl(dir, `${rel}.minisig`));
  return { body, sig };
}

async function fetchCoinRpc(base: string, method: string, body: unknown): Promise<unknown> {
  const url = `${base.replace(/\/+$/, "")}/${method}`;
  const res = await fetch(url, {
    method: "POST",
    mode: "cors",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await res.text();
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(`${url}: HTTP ${res.status}, not JSON`);
  }
}

function staleMessage(coin: CoinCheckJson): string | null {
  if (typeof coin.state === "object" && coin.state && "Stale" in coin.state) {
    return `The store coin was spent ${coin.state.Stale.blocks_ago} blocks ago, outside the grace window. A newer chain state may exist.`;
  }
  return null;
}

export async function loadCatalog(
  config: CatalogConfig,
  onProgress: (msg: string) => void,
): Promise<LoadedCatalog> {
  await initWasm();
  const dir = catalogDir(config.baseUrl, config.name);
  const verifier = new CatalogVerifier(config.name, config.pubkey);
  const warnings: string[] = [];
  const now = Date.now() / 1000;

  onProgress("Fetching signed head");
  const head = await fetchSigned(dir, "head.json");
  let headInfo: HeadInfo;
  try {
    headInfo = JSON.parse(verifier.verify_head(head.body, head.sig, now)) as HeadInfo;
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    if (config.mode === "strict") {
      return {
        config,
        verifier,
        status: { kind: "unverified", reason, warnings },
        entries: [],
        info: null,
      };
    }
    warnings.push(reason);
    return {
      config,
      verifier,
      status: { kind: "unverified", reason, warnings },
      entries: [],
      info: null,
    };
  }

  const remembered = loadAnchors()[config.name] as TrustAnchor | undefined;
  if (remembered && headInfo.revision < remembered.revision) {
    const reason = `rollback detected: already accepted revision ${remembered.revision}, offered ${headInfo.revision}`;
    if (config.mode === "strict") {
      return {
        config,
        verifier,
        status: { kind: "unverified", reason, warnings },
        entries: [],
        info: null,
      };
    }
    warnings.push(reason);
  } else if (
    remembered &&
    headInfo.revision === remembered.revision &&
    headInfo.root !== remembered.root
  ) {
    const reason = `fork detected at revision ${headInfo.revision}`;
    if (config.mode === "strict") {
      return {
        config,
        verifier,
        status: { kind: "unverified", reason, warnings },
        entries: [],
        info: null,
      };
    }
    warnings.push(reason);
  }

  try {
    if (headInfo.snapshot) {
      onProgress(`Loading snapshot at revision ${headInfo.snapshot.revision}`);
      const snapRel = headInfo.snapshot.path || snapshotPath(headInfo.snapshot.revision);
      const snap = await fetchSigned(dir, snapRel);
      verifier.load_snapshot(snap.body, snap.sig);
    }
    const from = Number(verifier.revision) + 1;
    const to = headInfo.revision;
    for (let rev = from; rev <= to; rev++) {
      onProgress(`Applying delta ${rev} of ${to}`);
      const delta = await fetchSigned(dir, deltaPath(rev));
      verifier.apply_delta(delta.body, delta.sig);
    }
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    if (config.mode === "strict") {
      return {
        config,
        verifier,
        status: { kind: "unverified", reason, warnings },
        entries: [],
        info: null,
      };
    }
    warnings.push(reason);
  }

  if (Number(verifier.revision) > 0) {
    saveAnchor(config.name, { revision: Number(verifier.revision), root: verifier.root });
  }

  let proof: VerifiedProof | undefined;
  let coin: CoinCheckJson | undefined;

  if (config.mode !== "off" && config.storeId) {
    onProgress("Verifying the Chia chain proof");
    try {
      const proofBody = await fetchBytes(joinUrl(dir, "chain-proof.json"));
      proof = JSON.parse(verifier.verify_chain_proof(proofBody, config.storeId)) as VerifiedProof;
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      if (config.mode === "strict") {
        return fail(config, verifier, `chain proof: ${reason}`, warnings);
      }
      warnings.push(`chain proof: ${reason}`);
    }

    if (proof && config.coinApiUrls.length > 0) {
      onProgress("Checking the store coin on chain");
      const errors: string[] = [];
      for (const api of config.coinApiUrls) {
        try {
          const rec = await fetchCoinRpc(api, "get_coin_record_by_name", {
            name: `0x${proof.coin_id}`,
          });
          let peak: bigint | undefined;
          const recObj = rec as { coin_record?: { spent?: boolean; spent_block_index?: number } };
          const spent =
            recObj.coin_record?.spent || (recObj.coin_record?.spent_block_index ?? 0) > 0;
          if (spent) {
            const state = (await fetchCoinRpc(api, "get_blockchain_state", {})) as {
              blockchain_state?: { peak?: { height?: number } };
            };
            const height = state.blockchain_state?.peak?.height;
            if (typeof height === "number") peak = BigInt(height);
          }
          coin = JSON.parse(
            check_coin_rpc(
              JSON.stringify(proof),
              JSON.stringify(rec),
              peak,
              BigInt(DEFAULT_GRACE_BLOCKS),
            ),
          ) as CoinCheckJson;
          const stale = staleMessage(coin);
          if (stale) {
            if (config.mode === "strict") {
              return fail(config, verifier, stale, warnings, proof, coin);
            }
            warnings.push(stale);
          }
          break;
        } catch (err) {
          errors.push(`${api}: ${err instanceof Error ? err.message : String(err)}`);
        }
      }
      if (!coin) {
        const reason = `chain unreachable: ${errors.join("; ") || "no coin API configured"}`;
        if (config.mode === "strict") {
          return fail(config, verifier, reason, warnings, proof);
        }
        warnings.push(reason);
      }
    }
  } else if (config.mode === "off") {
    warnings.push("Chain checks are off for this catalog.");
  }

  const entries = listEntries(verifier);
  const infoRaw = verifier.get_entry("ts/v1/catalog");
  let info: LoadedCatalog["info"] = null;
  if (infoRaw) {
    try {
      const parsed = JSON.parse(infoRaw) as {
        name?: string;
        publisher?: { display_name?: string; contact?: string; policy_url?: string };
      };
      info = {
        name: parsed.name,
        display_name: parsed.publisher?.display_name,
        contact: parsed.publisher?.contact,
        policy_url: parsed.publisher?.policy_url,
      };
    } catch {
      info = null;
    }
  }

  const verified = config.mode === "off" || (proof && coin && warnings.length === 0) || (proof && coin && config.mode === "warn");
  // In warn mode, presence of warnings still lists entries but status is unverified if chain failed.
  const chainOk = config.mode === "off" || (!!proof && !!coin && !warnings.some((w) => /chain proof|chain unreachable|rollback|fork|spent/.test(w)));
  const kind = chainOk && warnings.filter((w) => !w.startsWith("Chain checks")).length === 0 ? "verified" : config.mode === "strict" ? "unverified" : warnings.length ? (chainOk ? "verified" : "unverified") : "verified";

  const status: VerifyStatus = kind === "verified"
    ? {
        kind: "verified",
        revision: Number(verifier.revision),
        root: verifier.root,
        expiresUnix: headInfo.expires_unix,
        proof,
        coin,
        warnings,
      }
    : {
        kind: "unverified",
        reason: warnings[0] || "Verification did not complete.",
        warnings,
        revision: Number(verifier.revision),
        root: verifier.root,
      };

  // If we got here in strict with failures, fail() already returned.
  const _ = verified;
  return { config, verifier, status, entries, info };
}

function fail(
  config: CatalogConfig,
  verifier: CatalogVerifier,
  reason: string,
  warnings: string[],
  proof?: VerifiedProof,
  coin?: CoinCheckJson,
): LoadedCatalog {
  const _p = proof;
  const _c = coin;
  return {
    config,
    verifier,
    status: {
      kind: "unverified",
      reason,
      warnings: [...warnings, reason].filter((v, i, a) => a.indexOf(v) === i),
      revision: Number(verifier.revision) || undefined,
      root: verifier.root || undefined,
    },
    entries: [],
    info: null,
  };
}

export function listEntries(verifier: CatalogVerifier): ListedEntry[] {
  const keys: string[] = verifier.list_keys("ts/v1/index/");
  const out: ListedEntry[] = [];
  for (const indexKey of keys) {
    const parts = indexKey.split("/");
    // ts/v1/index/{type}/{org}/{name...}
    if (parts.length < 6) continue;
    const repoType = parts[3] === "dataset" ? "dataset" : parts[3] === "model" ? "model" : null;
    if (!repoType) continue;
    const id = parts.slice(4).join("/");
    const raw = verifier.get_entry(indexKey);
    if (!raw) continue;
    try {
      const index = JSON.parse(raw) as IndexEntry;
      out.push({
        id,
        repoType,
        indexKey,
        latestKey: `ts/v1/repo/${repoType}/${id}/latest`,
        index,
      });
    } catch {
      continue;
    }
  }
  out.sort((a, b) => a.id.localeCompare(b.id));
  return out;
}

export function loadManifest(verifier: CatalogVerifier, entry: ListedEntry): Manifest | null {
  const latestRaw = verifier.get_entry(entry.latestKey);
  let commit = entry.index.latest_commit;
  if (latestRaw) {
    try {
      const latest = JSON.parse(latestRaw) as { commit?: string; manifest_key?: string };
      if (latest.commit) commit = latest.commit;
      if (latest.manifest_key) {
        const raw = verifier.get_entry(latest.manifest_key);
        if (raw) return JSON.parse(wasm_parse_manifest(raw)) as Manifest;
      }
    } catch {
      // fall through to constructed key
    }
  }
  const key = `ts/v1/repo/${entry.repoType}/${entry.id}/rev/${commit}`;
  const raw = verifier.get_entry(key);
  if (!raw) return null;
  return JSON.parse(wasm_parse_manifest(raw)) as Manifest;
}

export function agreements(catalogs: LoadedCatalog[]): CatalogAgreement[] {
  const byId = new Map<string, { catalog: string; commit: string; hash: string }[]>();
  for (const c of catalogs) {
    if (c.status.kind === "unverified" && c.entries.length === 0) continue;
    for (const e of c.entries) {
      const manifest = loadManifest(c.verifier, e);
      const hash = manifest?.hf_commit ?? e.index.latest_commit;
      const list = byId.get(e.id) ?? [];
      list.push({ catalog: c.config.displayName || c.config.name, commit: hash, hash });
      byId.set(e.id, list);
    }
  }
  const out: CatalogAgreement[] = [];
  for (const [id, rows] of byId) {
    if (rows.length < 2) continue;
    for (let i = 0; i < rows.length; i++) {
      for (let j = i + 1; j < rows.length; j++) {
        out.push({
          id,
          same: rows[i].hash === rows[j].hash,
          left: rows[i].hash,
          right: rows[j].hash,
          leftCatalog: rows[i].catalog,
          rightCatalog: rows[j].catalog,
        });
      }
    }
  }
  return out;
}
