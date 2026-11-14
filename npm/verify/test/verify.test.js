// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Node tests for @tensorseed/verify against the vectors that
// crates/tensorseed-wasm/tests/npm_fixtures.rs generates. Every key in
// them is a throwaway test key.

import { test } from "node:test";
import assert from "node:assert/strict";
import { createPrivateKey, sign } from "node:crypto";
import { readFileSync } from "node:fs";

import * as v from "../node.js";

const x = JSON.parse(readFileSync(new URL("./fixtures/vectors.json", import.meta.url), "utf8"));
const NOW = x.catalog.now_unix; // 2026-11-20T00:00:00Z
const DAY = 86400;
const at = (iso) => Math.floor(Date.parse(iso) / 1000);
const claim = JSON.parse(x.claim);

const check = (r, name) => r.checks.find((c) => c.name === name);
const statuses = (r) => Object.fromEntries(r.checks.map((c) => [c.name, c.status]));

function catalog() {
  const { verifier, head } = v.openCatalog({ ...x.catalog, now: NOW });
  assert.equal(head.status, "up_to_date");
  return verifier;
}

// The fixtures' response key is the Ed25519 seed [7; 32].
function responseKey() {
  const seed = Buffer.alloc(32, 7);
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]);
  return createPrivateKey({ key: der, format: "der", type: "pkcs8" });
}

function revocationList(reason, revokedAt) {
  const msg = v.revocationSigningMessage(claim.claim_sha256, revokedAt, reason);
  const sig = sign(null, Buffer.from(msg, "utf8"), responseKey()).toString("hex");
  return {
    schema: "ts.claim-revocations/1",
    revocations: [
      { claim_sha256: claim.claim_sha256, revoked_at: revokedAt, reason, response_sig: sig },
    ],
  };
}

const response = (extra = {}) => ({
  claim: x.claim,
  headers: x.response.headers,
  body: x.response.body,
  requestId: x.response.request_id,
  now: x.response.now_unix,
  ...extra,
});

test("a claim checks out against a verified catalog", () => {
  const r = v.checkClaimWithCatalog(catalog(), x.claim, {
    originUrl: x.claim_url,
    now: NOW,
    revocationsAbsent: "test",
    requireWalletSig: true,
    requireExpiry: true,
  });
  assert.equal(r.verdict, "pass", JSON.stringify(r.checks));
  assert.equal(r.schema, "ts.claim-check/1");
  assert.equal(r.wallet_verified, true);
  assert.equal(r.seller_wallet, x.seller_wallet);
  assert.equal(r.expires_at, "2026-12-13T00:00:00Z");
  assert.equal(r.revoked, false);
  assert.equal(check(r, "freshness").status, "pass");
  assert.equal(check(r, "manifest").status, "pass");
});

test("without a catalog the best verdict is inconclusive", () => {
  const r = v.checkClaim(x.claim, { originUrl: x.claim_url, now: NOW });
  assert.equal(r.verdict, "inconclusive");
  assert.equal(check(r, "claim_sha256").status, "pass");
  assert.equal(check(r, "wallet_signature").status, "pass");
  assert.equal(check(r, "catalog").status, "skip");
  assert.equal(check(r, "revocation").status, "skip");
});

test("a claim object, bytes, and a string give the same report", () => {
  const o = { originUrl: x.claim_url, now: NOW };
  const a = v.checkClaim(x.claim, o);
  assert.deepEqual(v.checkClaim(claim, o), a);
  assert.deepEqual(v.checkClaim(new TextEncoder().encode(x.claim), o), a);
});

test("a tampered claim fails its hash", () => {
  const bad = { ...claim, served_model_id: "org/other" };
  const r = v.checkClaimWithCatalog(catalog(), bad, { originUrl: x.claim_url, now: NOW });
  assert.equal(r.verdict, "fail");
  assert.equal(check(r, "claim_sha256").status, "fail");
});

test("a claim copied to another host fails the public URL check", () => {
  const r = v.checkClaim(x.claim, {
    originUrl: "https://copy.example.net/.well-known/tensorseed/served-weights.json",
    now: NOW,
  });
  assert.equal(r.verdict, "fail");
  assert.equal(check(r, "public_url").status, "fail");
});

test("a claim fails after expires_at", () => {
  const ok = v.checkClaim(x.claim, { originUrl: x.claim_url, now: at("2026-12-12T23:59:00Z") });
  assert.equal(check(ok, "expiry").status, "pass");
  const r = v.checkClaim(x.claim, { originUrl: x.claim_url, now: at("2026-12-13T00:00:01Z") });
  assert.equal(r.verdict, "fail");
  assert.equal(check(r, "expiry").status, "fail");
});

test("stripping expires_at from a claim breaks its hash", () => {
  // expires_at is covered by claim_sha256 and the wallet signature, so a
  // copy with the expiry removed cannot pass for a claim that never had one.
  const noExpiry = { ...claim };
  delete noExpiry.expires_at;
  const r = v.checkClaim(noExpiry, { originUrl: x.claim_url, now: NOW, requireExpiry: true });
  assert.equal(r.verdict, "fail");
  assert.equal(check(r, "claim_sha256").status, "fail");
  assert.equal(check(r, "expiry").status, "skip");
});

test("a pending revocation warns, and an effective one fails", () => {
  const pending = v.checkClaim(x.claim, {
    originUrl: x.claim_url,
    now: NOW,
    revocations: x.revocations,
    revocationsFrom: "test",
  });
  assert.equal(check(pending, "revocation").status, "warn");
  assert.equal(pending.revoked, false);

  const later = NOW + 6 * DAY; // after 2026-11-25
  const r = v.checkClaimWithCatalog(catalog(), x.claim, {
    originUrl: x.claim_url,
    now: later,
    revocations: x.revocations,
    revocationsFrom: "test",
  });
  assert.equal(r.verdict, "fail");
  assert.equal(r.revoked, true);
  assert.equal(check(r, "revocation").status, "fail");
});

test("a revocation signed in JavaScript verifies, a forged one is ignored", () => {
  const list = revocationList("stopped", "2026-11-19T00:00:00Z");
  const r = v.checkClaim(x.claim, { originUrl: x.claim_url, now: NOW, revocations: list });
  assert.equal(check(r, "revocation").status, "fail");

  const forged = structuredClone(list);
  forged.revocations[0].reason = "other";
  const f = v.checkClaim(x.claim, { originUrl: x.claim_url, now: NOW, revocations: forged });
  assert.notEqual(check(f, "revocation").status, "fail");
});

test("an unreachable revocation list is not a pass", () => {
  const r = v.checkClaim(x.claim, {
    originUrl: x.claim_url,
    now: NOW,
    revocationsError: "HTTP 500",
  });
  assert.equal(check(r, "revocation").status, "warn");
});

test("a signed response verifies", () => {
  const r = v.verifyResponse(response());
  assert.equal(r.valid, true, JSON.stringify(r));
  assert.equal(r.claim_sha256, claim.claim_sha256);
  assert.equal(r.seller_wallet, x.seller_wallet);
  assert.equal(r.served_model_id, "org/tiny");
});

test("response headers may be a Headers object or a plain object", () => {
  assert.equal(v.verifyResponse(response({ headers: new Headers(x.response.headers) })).valid, true);
  const obj = Object.fromEntries(x.response.headers);
  assert.equal(v.verifyResponse(response({ headers: obj })).valid, true);
  assert.equal(v.verifyResponse(response({ body: new TextEncoder().encode(x.response.body) })).valid, true);
});

test("a tampered response body fails", () => {
  const r = v.verifyResponse(response({ body: x.response.body.replace("chatcmpl-1", "chatcmpl-2") }));
  assert.equal(r.valid, false);
  assert.equal(r.error, "body_hash_mismatch");
});

test("a replayed response fails the request id check", () => {
  const r = v.verifyResponse(response({ requestId: "req-2" }));
  assert.equal(r.valid, false);
  assert.equal(r.error, "request_id_mismatch");
});

test("a response for another model fails", () => {
  const r = v.verifyResponse(response({ requestedModel: "org/other" }));
  assert.equal(r.valid, false);
});

test("a stale response fails the clock check", () => {
  const r = v.verifyResponse(response({ now: NOW + 3600 }));
  assert.equal(r.valid, false);
  assert.equal(r.error, "timestamp_skew");
});

test("a response signed before a revocation still verifies", () => {
  const r = v.verifyResponse(response({ revocations: x.revocations }));
  assert.equal(r.valid, true, JSON.stringify(r));
});

test("a response signed after a revocation fails", () => {
  const r = v.verifyResponse(response({ revocations: revocationList("stopped", "2026-11-19T00:00:00Z") }));
  assert.equal(r.valid, false);
  assert.equal(r.error, "claim_revoked");
});

test("key_compromised revokes every response under the claim", () => {
  const r = v.verifyResponse(response({ revocations: revocationList("key_compromised", "2026-12-01T00:00:00Z") }));
  assert.equal(r.valid, false);
  assert.equal(r.error, "claim_revoked");
});

test("the wallet signature recovers to the seller wallet", () => {
  const msg = v.claimSigningMessage(claim.claim_sha256);
  assert.equal(msg, `tensorseed served-weights/1:\n${claim.claim_sha256}`);
  assert.equal(v.recoverWallet(msg, claim.sig.value), x.seller_wallet);
});

test("claimUrl and revocationsUrl follow the well-known layout", () => {
  assert.equal(v.claimUrl("https://llm.example.com/v1/"), x.claim_url);
  assert.equal(v.claimUrl("https://llm.example.com"), x.claim_url);
  assert.equal(
    v.revocationsUrl(x.claim_url),
    "https://llm.example.com/.well-known/tensorseed/revocations.json",
  );
  assert.throws(() => v.claimUrl("ftp://llm.example.com"));
  assert.throws(() => v.claimUrl("https://user:pw@llm.example.com"));
});

test("checkEndpoint fetches the claim and the revocation list", async () => {
  const seen = [];
  const fakeFetch = async (url) => {
    seen.push(url);
    if (url === x.claim_url) return new Response(x.claim, { status: 200 });
    return new Response(x.revocations, { status: 200 });
  };
  const r = await v.checkEndpoint("https://llm.example.com/v1", {
    fetch: fakeFetch,
    now: NOW + 6 * DAY,
    catalog: catalog(),
  });
  assert.deepEqual(seen, [x.claim_url, "https://llm.example.com/.well-known/tensorseed/revocations.json"]);
  assert.equal(r.revoked, true);
  assert.equal(r.verdict, "fail");

  const absent = await v.checkEndpoint("https://llm.example.com/v1", {
    fetch: async (url) =>
      url === x.claim_url ? new Response(x.claim) : new Response("", { status: 404 }),
    now: NOW,
    catalog: catalog(),
  });
  assert.equal(absent.verdict, "pass", JSON.stringify(statuses(absent)));
});

test("the browser entry works once initialized with the wasm bytes", async () => {
  const web = await import("../index.js");
  assert.throws(() => web.checkClaim(x.claim), /await init/);
  await web.init(readFileSync(new URL("../pkg/tensorseed_wasm_bg.wasm", import.meta.url)));
  const r = web.verifyResponse(response());
  assert.equal(r.valid, true);
});
