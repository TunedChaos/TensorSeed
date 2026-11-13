// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "unknown size";
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let u = -1;
  do {
    v /= 1024;
    u += 1;
  } while (v >= 1024 && u < units.length - 1);
  const digits = v >= 10 || u === 0 ? 0 : 1;
  return `${v.toFixed(digits)} ${units[u]}`;
}

export function shortHex(s: string, head = 8, tail = 4): string {
  if (s.length <= head + tail + 1) return s;
  return `${s.slice(0, head)}…${s.slice(-tail)}`;
}

export function joinUrl(base: string, ...parts: string[]): string {
  let out = base.replace(/\/+$/, "");
  for (const p of parts) {
    const bit = p.replace(/^\/+/, "");
    if (bit) out += `/${bit}`;
  }
  return out;
}

export function catalogDir(baseUrl: string, name: string): string {
  return joinUrl(baseUrl, name) + "/";
}

export function pullCommand(
  id: string,
  repoType: "model" | "dataset",
  includes: string[],
): string {
  const parts = ["tensorseed", "pull", id];
  if (repoType === "dataset") parts.push("--type", "dataset");
  for (const inc of includes) {
    parts.push("--include", inc);
  }
  return parts.join(" ");
}

export function sizeBucket(bytes: number): "tiny" | "small" | "medium" | "large" {
  if (bytes < 10 * 1024 * 1024) return "tiny";
  if (bytes < 100 * 1024 * 1024) return "small";
  if (bytes < 1024 * 1024 * 1024) return "medium";
  return "large";
}

export function expiresLabel(expiresUnix: number): string {
  const d = new Date(expiresUnix * 1000);
  if (Number.isNaN(d.getTime())) return "unknown";
  return d.toISOString().replace(/\.\d+Z$/, "Z");
}
