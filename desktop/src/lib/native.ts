// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// The native CatalogSource: every call is a Tauri command in
// src-tauri/src/commands.rs, so verification runs in Rust, not here.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CatalogEntries, CatalogSource, CatalogSummary } from "@tensorseed/ui/source";
import type { ThemePreference } from "@tensorseed/ui/types";

export type CatalogsList = { configPath: string; catalogs: CatalogSummary[] };

export function catalogsList(): Promise<CatalogsList> {
  return invoke<CatalogsList>("catalogs_list");
}

export const nativeSource: CatalogSource = {
  async listCatalogs() {
    return (await catalogsList()).catalogs;
  },
  loadEntries(name, { sync }) {
    return invoke<CatalogEntries>("entries_list", { name, sync });
  },
};

/** Must match `DEEP_LINK_EVENT` in src-tauri/src/lib.rs. */
export function onDeepLink(handler: (url: string) => void): Promise<UnlistenFn> {
  return listen<string>("deep-link://open", (e) => handler(e.payload));
}

const THEME_KEY = "tensorseed.desktop.theme";

export function loadTheme(): ThemePreference {
  const t = localStorage.getItem(THEME_KEY);
  return t === "light" || t === "dark" || t === "system" ? t : "system";
}

export function saveTheme(theme: ThemePreference): void {
  localStorage.setItem(THEME_KEY, theme);
}
