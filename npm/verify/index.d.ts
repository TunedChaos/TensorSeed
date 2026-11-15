// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

export type Json = string | Uint8Array | ArrayBuffer | object;
export type Bytes = string | Uint8Array | ArrayBuffer;
/** A Date, or Unix seconds. Defaults to the current time. */
export type When = Date | number;

export const WELL_KNOWN_CLAIM: string;
export const WELL_KNOWN_REVOCATIONS: string;

/** Load the wasm. Required once in a browser; a no-op in Node. */
export function init(wasm?: string | URL | Request | Response | BufferSource | WebAssembly.Module): Promise<void>;

export interface ClaimCheckOptions {
  /** The URL the claim was fetched from. */
  originUrl?: string;
  /** Or the local file name it was read from. */
  originFile?: string;
  /** The endpoint base URL the buyer was given. */
  expectUrl?: string;
  requireWalletSig?: boolean;
  /** Seller wallet the claim must name (0x address). */
  expectWallet?: string;
  /** Fail, instead of warn, when the claim has no expires_at. */
  requireExpiry?: boolean;
  now?: When;
  /** A ts.claim-revocations/1 list (merge your own with the seller's). */
  revocations?: Json;
  /** Where the list came from, for the report. */
  revocationsFrom?: string;
  /** The seller's list URL returned 404. */
  revocationsAbsent?: string;
  /** The seller's list could not be read: a passing verdict becomes inconclusive. */
  revocationsError?: string;
  /** The seller's list as fetched, merged with `revocations`; one that does not parse counts as `revocationsError`. */
  sellerRevocations?: Json;
  /** Where the seller's list came from, for the report. */
  sellerRevocationsFrom?: string;
}

export type CheckStatus = "pass" | "fail" | "warn" | "skip";

export interface ClaimCheckReport {
  schema: "ts.claim-check/1";
  source: string;
  claim_sha256?: string;
  repo?: string;
  revision?: string;
  served_model_id?: string;
  public_base_url?: string;
  expires_at?: string | null;
  revoked?: boolean;
  seller_wallet?: string;
  wallet_verified?: boolean;
  response_pubkey?: string;
  checks: { name: string; status: CheckStatus; detail: string }[];
  verdict: "pass" | "fail" | "inconclusive";
  counts: Record<CheckStatus, number>;
  [key: string]: unknown;
}

export interface VerifyResponseInput {
  claim: Json;
  /** A Headers object, [name, value] pairs, or a plain object. */
  headers: Headers | [string, string][] | Record<string, string>;
  /** The exact response body bytes (the stream without the SSE trailer). */
  body: Bytes;
  /** The X-Tensorseed-Request-Id you sent. */
  requestId?: string;
  /** The model you asked for. */
  requestedModel?: string;
  /** The checking time (default now). For a stored response, pass the time you received it. */
  now?: When;
  /** Allowed timestamp skew either way, 1 to 604800 seconds (default 300). */
  maxSkewSecs?: number;
  revocations?: Json;
}

export type VerifyResponseResult =
  | {
      valid: true;
      claim_sha256: string;
      claim_expires_at: string | null;
      seller_wallet: string;
      served_model_id: string;
      request_id: string;
      timestamp: string;
      body_sha256: string;
    }
  | { valid: false; error: string; detail: string };

export class CatalogVerifier {
  constructor(catalog: string, pubkey: string);
  readonly catalog: string;
  readonly revision: bigint;
  readonly root: string;
  readonly entry_count: number;
  verify_head(body: Uint8Array, sig: string, now_unix: number): string;
  apply_delta(body: Uint8Array, sig: string): void;
  load_snapshot(body: Uint8Array, sig: string): void;
  get_entry(key: string): string;
  check_claim(claim_json: string, options_json: string): string;
  list_keys(prefix: string): string[];
  free(): void;
}

export interface SignedFile {
  body: Bytes;
  sig: string;
}

export function openCatalog(input: {
  name: string;
  /** The publisher's minisign public key. */
  pubkey: string;
  snapshot?: SignedFile;
  deltas?: SignedFile[];
  head?: SignedFile;
  now?: When;
}): { verifier: CatalogVerifier; head?: { status: string; [key: string]: unknown } };

export function checkClaim(claim: Json, options?: ClaimCheckOptions): ClaimCheckReport;
export function checkClaimWithCatalog(
  verifier: CatalogVerifier,
  claim: Json,
  options?: ClaimCheckOptions,
): ClaimCheckReport;
export function checkEndpoint(
  baseUrl: string,
  options?: ClaimCheckOptions & {
    fetch?: typeof fetch;
    catalog?: CatalogVerifier;
    /** Skip the seller's revocation list (accept one that cannot be read). */
    noRevocations?: boolean;
  },
): Promise<ClaimCheckReport>;
export function verifyResponse(input: VerifyResponseInput): VerifyResponseResult;

export function claimUrl(baseUrl: string): string;
export function revocationsUrl(claimUrl: string): string | undefined;
export function claimSigningMessage(claimSha256: string): string;
export function revocationSigningMessage(claimSha256: string, revokedAt: string, reason: string): string;
/** The 0x address that signed `message` (EIP-191), lowercase. */
export function recoverWallet(message: string, signature: string): string;
