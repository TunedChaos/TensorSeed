// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

/** Official Tensorseed catalog, compiled into the page (SPEC 8.4). */
export const OFFICIAL_CATALOG = {
  name: "tensorseed",
  displayName: "Tensorseed (official)",
  baseUrl: "https://catalog.tensorseed.dev/",
  pubkey: "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd",
  storeId: "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76",
  coinApiUrls: ["https://api.coinset.org"],
  mode: "strict" as const,
};

export const DEFAULT_GRACE_BLOCKS = 192;
export const MAX_ARTIFACT_BYTES = 256 * 1024 * 1024;
