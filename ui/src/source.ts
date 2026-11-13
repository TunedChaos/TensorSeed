// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import type { ListedEntry, VerifyMode } from "./types";

/**
 * Where catalog data and verification come from. The explorer verifies in
 * WebAssembly in the page; Desktop verifies natively through Tauri commands.
 * Shared views depend on this shape, not on either implementation.
 */
export interface CatalogSource {
  /** Subscribed catalogs, default first. */
  listCatalogs(): Promise<CatalogSummary[]>;
  /** One catalog's entries plus how far its verification got. */
  loadEntries(name: string, opts: { sync: boolean }): Promise<CatalogEntries>;
}

export type CatalogSummary = {
  name: string;
  label: string;
  baseUrl: string;
  storeId: string;
  backend: string;
  chainCheck: VerifyMode;
  priority: number;
  isDefault: boolean;
  /** Locally accepted revision, or null when nothing is synced yet. */
  localRevision: number | null;
  error: string | null;
};

export type ChainView =
  | { kind: "off" }
  | {
      kind: "anchored";
      storeId: string;
      datalayerRoot: string;
      coinId: string;
      coinState: string;
      via: string;
      checkedUnix: number;
    }
  | { kind: "datalayer"; storeId: string }
  | { kind: "unchecked"; reason: string };

export type CatalogEntries = {
  catalog: CatalogSummary;
  revision: number;
  root: string;
  synced: boolean;
  syncError: string | null;
  chain: ChainView;
  entries: ListedEntry[];
  /** Index values that did not parse; shown, never trusted. */
  problems: string[];
};
