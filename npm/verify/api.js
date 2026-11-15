// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// The JavaScript API of @tensorseed/verify, shared by the browser entry
// (index.js) and the Node entry (node.js). Every check runs in the
// WebAssembly build of the Rust verify-only crate; this file only converts
// arguments and parses results.

export const WELL_KNOWN_CLAIM = "/.well-known/tensorseed/served-weights.json";
export const WELL_KNOWN_REVOCATIONS = "/.well-known/tensorseed/revocations.json";
const MAX_DOC_BYTES = 4 * 1024 * 1024;

function text(v, what) {
  if (typeof v === "string") return v;
  if (v instanceof Uint8Array) return new TextDecoder().decode(v);
  if (v instanceof ArrayBuffer) return new TextDecoder().decode(new Uint8Array(v));
  if (v && typeof v === "object") return JSON.stringify(v);
  throw new TypeError(`${what} must be a JSON string, bytes, or an object`);
}

function bytes(v, what) {
  if (typeof v === "string") return new TextEncoder().encode(v);
  if (v instanceof Uint8Array) return v;
  if (v instanceof ArrayBuffer) return new Uint8Array(v);
  throw new TypeError(`${what} must be a string, a Uint8Array, or an ArrayBuffer`);
}

function unix(now) {
  if (now === undefined || now === null) return Math.floor(Date.now() / 1000);
  if (now instanceof Date) return Math.floor(now.getTime() / 1000);
  if (typeof now === "number" && Number.isFinite(now)) return Math.floor(now);
  throw new TypeError("now must be a Date or Unix seconds");
}

function headerPairs(h) {
  if (!h) return [];
  if (Array.isArray(h)) return h.map(([k, v]) => [String(k), String(v)]);
  if (typeof h.forEach === "function" && typeof h.get === "function") {
    const out = [];
    h.forEach((v, k) => out.push([k, v]));
    return out;
  }
  return Object.entries(h).map(([k, v]) => [k, String(v)]);
}

function revocationsText(r) {
  return r === undefined || r === null ? undefined : text(r, "revocations");
}

function claimOptions(o = {}) {
  const out = {
    now_unix: unix(o.now),
    require_wallet_sig: Boolean(o.requireWalletSig),
    require_expiry: Boolean(o.requireExpiry),
  };
  if (o.originUrl !== undefined) out.origin_url = o.originUrl;
  if (o.originFile !== undefined) out.origin_file = o.originFile;
  if (o.expectUrl !== undefined) out.expect_url = o.expectUrl;
  if (o.expectWallet !== undefined) out.expect_wallet = o.expectWallet;
  const r = revocationsText(o.revocations);
  if (r !== undefined) out.revocations = r;
  if (o.revocationsFrom !== undefined) out.revocations_from = o.revocationsFrom;
  if (o.revocationsAbsent !== undefined) out.revocations_absent = o.revocationsAbsent;
  if (o.revocationsError !== undefined) out.revocations_error = o.revocationsError;
  const s = revocationsText(o.sellerRevocations);
  if (s !== undefined) out.seller_revocations = s;
  if (o.sellerRevocationsFrom !== undefined) out.seller_revocations_from = o.sellerRevocationsFrom;
  return JSON.stringify(out);
}

/** Where a seller publishes its claim, for an endpoint base URL. */
export function claimUrl(baseUrl) {
  const u = new URL(baseUrl);
  if (u.protocol !== "https:" && u.protocol !== "http:") {
    throw new TypeError(`${baseUrl} must be an http or https URL`);
  }
  if (u.username || u.password) throw new TypeError("do not put credentials in the URL");
  if (u.pathname.endsWith(".json")) return u.toString();
  let root = `${u.origin}${u.pathname}`.replace(/\/+$/, "");
  if (root.endsWith("/v1")) root = root.slice(0, -3);
  return `${root}${WELL_KNOWN_CLAIM}`;
}

/** The revocation list next to a claim read from its well-known path. */
export function revocationsUrl(claimUrlString) {
  const u = new URL(claimUrlString);
  if (!u.pathname.endsWith(WELL_KNOWN_CLAIM)) return undefined;
  u.pathname = u.pathname.slice(0, -WELL_KNOWN_CLAIM.length) + WELL_KNOWN_REVOCATIONS;
  u.search = "";
  u.hash = "";
  return u.toString();
}

async function readCapped(resp, url) {
  const buf = new Uint8Array(await resp.arrayBuffer());
  if (buf.length > MAX_DOC_BYTES) throw new Error(`${url} is larger than ${MAX_DOC_BYTES} bytes`);
  return new TextDecoder().decode(buf);
}

export function makeApi(raw, isReady) {
  const ready = () => {
    if (!isReady()) {
      throw new Error("@tensorseed/verify is not initialized; await init() first");
    }
  };

  /**
   * Check a claim without a catalog. The best verdict is "inconclusive";
   * use checkClaimWithCatalog for the full check.
   */
  function checkClaim(claim, options = {}) {
    ready();
    return JSON.parse(raw.check_claim(text(claim, "claim"), claimOptions(options)));
  }

  /** Check a claim against a CatalogVerifier that holds a verified state. */
  function checkClaimWithCatalog(verifier, claim, options = {}) {
    ready();
    return JSON.parse(verifier.check_claim(text(claim, "claim"), claimOptions(options)));
  }

  /**
   * Build a CatalogVerifier from a snapshot (or deltas) and check the
   * signed head against it, so freshness counts as synced.
   */
  function openCatalog({ name, pubkey, snapshot, deltas = [], head, now }) {
    ready();
    const v = new raw.CatalogVerifier(name, pubkey);
    if (snapshot) v.load_snapshot(bytes(snapshot.body, "snapshot.body"), snapshot.sig);
    for (const d of deltas) v.apply_delta(bytes(d.body, "delta.body"), d.sig);
    let headStatus;
    if (head) {
      headStatus = JSON.parse(v.verify_head(bytes(head.body, "head.body"), head.sig, unix(now)));
    }
    return { verifier: v, head: headStatus };
  }

  /** Verify a signed response (headers and the exact body bytes). */
  function verifyResponse({
    claim,
    headers,
    body,
    requestId,
    requestedModel,
    now,
    maxSkewSecs,
    revocations,
  }) {
    ready();
    const o = { now_unix: unix(now) };
    if (requestId !== undefined) o.request_id = requestId;
    if (requestedModel !== undefined) o.requested_model = requestedModel;
    if (maxSkewSecs !== undefined) o.max_skew_secs = maxSkewSecs;
    const r = revocationsText(revocations);
    if (r !== undefined) o.revocations = r;
    return JSON.parse(
      raw.verify_response(
        text(claim, "claim"),
        JSON.stringify(headerPairs(headers)),
        bytes(body, "body"),
        JSON.stringify(o),
      ),
    );
  }

  /**
   * Fetch an endpoint's claim and the seller's revocation list with fetch,
   * then check them (against `catalog`, a CatalogVerifier, when given).
   * The seller's list is merged with `revocations`; one that cannot be
   * fetched or does not parse makes a passing verdict inconclusive.
   * `noRevocations: true` skips the seller's list.
   */
  async function checkEndpoint(baseUrl, options = {}) {
    ready();
    const f = options.fetch ?? globalThis.fetch;
    if (typeof f !== "function") throw new Error("no fetch available; pass options.fetch");
    const url = claimUrl(baseUrl);
    const resp = await f(url, { redirect: "follow" });
    if (!resp.ok) throw new Error(`fetching ${url}: HTTP ${resp.status}`);
    const finalUrl = resp.url || url;
    const claim = await readCapped(resp, finalUrl);
    const o = { ...options, originUrl: finalUrl };
    delete o.fetch;
    delete o.catalog;
    delete o.noRevocations;
    const revUrl = revocationsUrl(finalUrl);
    if (!options.noRevocations && revUrl) {
      try {
        const r = await f(revUrl, { redirect: "follow" });
        if (r.status === 404) o.revocationsAbsent = revUrl;
        else if (!r.ok) o.revocationsError = `${revUrl}: HTTP ${r.status}`;
        else {
          // A list that does not parse makes the run inconclusive.
          o.sellerRevocations = await readCapped(r, revUrl);
          o.sellerRevocationsFrom = revUrl;
        }
      } catch (e) {
        o.revocationsError = `${revUrl}: ${e.message ?? e}`;
      }
    }
    return options.catalog
      ? checkClaimWithCatalog(options.catalog, claim, o)
      : checkClaim(claim, o);
  }

  return {
    checkClaim,
    checkClaimWithCatalog,
    openCatalog,
    verifyResponse,
    checkEndpoint,
    claimSigningMessage: (claimSha256) => (ready(), raw.claim_signing_message(claimSha256)),
    revocationSigningMessage: (claimSha256, revokedAt, reason) => (
      ready(), raw.revocation_signing_message(claimSha256, revokedAt, reason)
    ),
    recoverWallet: (message, signature) => (ready(), raw.recover_wallet(message, signature)),
  };
}
