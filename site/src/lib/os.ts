// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import type { Os } from "#lib/release.ts";

export type DetectedOs = Os | "mac" | "other";

/** Best effort guess of the visitor's OS, only in the browser. */
export function detectOs(): DetectedOs {
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
  const p = `${nav.userAgentData?.platform ?? ""} ${navigator.platform ?? ""} ${navigator.userAgent}`.toLowerCase();
  if (p.includes("android") || p.includes("iphone") || p.includes("ipad")) return "other";
  if (p.includes("win")) return "windows";
  if (p.includes("mac")) return "mac";
  if (p.includes("linux") || p.includes("x11")) return "linux";
  return "other";
}
