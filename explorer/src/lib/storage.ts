// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import { OFFICIAL_CATALOG } from "./official";
import type { CatalogConfig, TrustAnchor, VerifyMode } from "./types";

const CATALOGS_KEY = "tensorseed.explorer.catalogs.v1";
const ANCHORS_KEY = "tensorseed.explorer.anchors.v1";
const THEME_KEY = "tensorseed.explorer.theme";
const ACTIVE_KEY = "tensorseed.explorer.active";

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

function writeJson(key: string, value: unknown): void {
  localStorage.setItem(key, JSON.stringify(value));
}

export function loadCatalogs(): CatalogConfig[] {
  const saved = readJson<CatalogConfig[]>(CATALOGS_KEY, []);
  const official = officialConfig();
  const rest = saved.filter((c) => c.name !== official.name);
  return [official, ...rest];
}

export function officialConfig(): CatalogConfig {
  return { ...OFFICIAL_CATALOG, coinApiUrls: [...OFFICIAL_CATALOG.coinApiUrls] };
}

export function saveCatalogs(catalogs: CatalogConfig[]): void {
  // Persist extras and any official-catalog overrides (mode, urls). The
  // compiled official pin is always merged back in on load.
  const official = officialConfig();
  const extras = catalogs.filter((c) => c.name !== official.name);
  const currentOfficial = catalogs.find((c) => c.name === official.name);
  const record: CatalogConfig[] = [...extras];
  if (
    currentOfficial &&
    (currentOfficial.mode !== official.mode ||
      currentOfficial.baseUrl !== official.baseUrl ||
      currentOfficial.pubkey !== official.pubkey ||
      currentOfficial.storeId !== official.storeId)
  ) {
    record.unshift(currentOfficial);
  }
  writeJson(CATALOGS_KEY, record);
}

export function loadAnchors(): Record<string, TrustAnchor> {
  return readJson(ANCHORS_KEY, {});
}

export function saveAnchor(catalog: string, anchor: TrustAnchor): void {
  const all = loadAnchors();
  all[catalog] = anchor;
  writeJson(ANCHORS_KEY, all);
}

export function loadTheme(): "light" | "dark" | "system" {
  const t = localStorage.getItem(THEME_KEY);
  return t === "light" || t === "dark" || t === "system" ? t : "system";
}

export function saveTheme(theme: "light" | "dark" | "system"): void {
  localStorage.setItem(THEME_KEY, theme);
}

export function loadActiveCatalog(): string {
  return localStorage.getItem(ACTIVE_KEY) || OFFICIAL_CATALOG.name;
}

export function saveActiveCatalog(name: string): void {
  localStorage.setItem(ACTIVE_KEY, name);
}

export function setOfficialMode(mode: VerifyMode, catalogs: CatalogConfig[]): CatalogConfig[] {
  return catalogs.map((c) => (c.name === OFFICIAL_CATALOG.name ? { ...c, mode } : c));
}
