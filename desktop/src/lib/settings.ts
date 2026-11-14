// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Client side checks for the [torrent] form. They catch typos as you type;
// the native side runs the same validation the CLI does before anything
// is written (settings_check and settings_set).

import type { TorrentSettings } from "./native";

export const DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] as const;
const GROUPS: Record<string, readonly string[]> = {
  weekdays: ["mon", "tue", "wed", "thu", "fri"],
  weekends: ["sat", "sun"],
  all: DAYS,
};
const LONG: Record<string, string> = {
  monday: "mon",
  tuesday: "tue",
  wednesday: "wed",
  thursday: "thu",
  friday: "fri",
  saturday: "sat",
  sunday: "sun",
};

/** The days a window's `days` list covers (empty means every day). */
export function expandDays(days: string[]): Set<string> {
  const out = new Set<string>();
  for (const raw of days) {
    const d = raw.trim().toLowerCase();
    for (const x of GROUPS[d] ?? [LONG[d] ?? d]) out.add(x);
  }
  return out;
}

export type Errors = Partial<Record<string, string>>;

const U32 = 4294967295;

function uint(e: Errors, key: string, v: unknown, max = Number.MAX_SAFE_INTEGER) {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > max) {
    e[key] = max === U32 ? "a whole number, 0 or more" : max === 65535 ? "a port from 0 to 65535" : "a whole number, 0 or more";
  }
}

function hhmm(s: string, allow24: boolean): boolean {
  const m = /^(\d{1,2}):(\d{2})$/.exec(s.trim());
  if (!m) return false;
  const h = Number(m[1]);
  const min = Number(m[2]);
  if (allow24 && h === 24 && min === 0) return true;
  return h < 24 && min < 60;
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;

function isIp(s: string): boolean {
  return IPV4.test(s) || (s.includes(":") && /^[0-9a-fA-F:.]+$/.test(s));
}

/** Field errors, keyed by field name (`alt_speed.window.0.start` for windows). */
export function validate(t: TorrentSettings, trackers: string[], peers: string[]): Errors {
  const e: Errors = {};
  for (const k of [
    "upload_limit_kbps",
    "download_limit_kbps",
    "torrent_upload_limit_kbps",
    "torrent_download_limit_kbps",
    "swarm_no_peer_secs",
    "swarm_stall_secs",
    "swarm_timeout_secs",
    "piece_length",
  ] as const) {
    uint(e, k, t[k]);
  }
  uint(e, "max_peers_per_torrent", t.max_peers_per_torrent, U32);
  uint(e, "concurrent_init_limit", t.concurrent_init_limit, U32);
  uint(e, "listen_port", t.listen_port, 65535);
  uint(e, "alt_speed.upload_limit_kbps", t.alt_speed.upload_limit_kbps);
  uint(e, "alt_speed.download_limit_kbps", t.alt_speed.download_limit_kbps);
  if (!isIp(t.listen_addr.trim())) e.listen_addr = "an IP address, for example 0.0.0.0";
  const pl = t.piece_length;
  if (!e.piece_length && pl !== 0 && (pl < 16384 || pl > 2 ** 30 || (pl & (pl - 1)) !== 0)) {
    e.piece_length = "0 (auto) or a power of two from 16384 to 1073741824";
  }
  if (typeof t.seed_ratio_limit !== "number" || !Number.isFinite(t.seed_ratio_limit) || t.seed_ratio_limit < 0) {
    e.seed_ratio_limit = "0 (no limit) or a positive number, for example 2.0";
  }
  const st = t.seed_time_limit.trim();
  if (st && st !== "forever" && !/^\d+(s|m|h|d)?$/.test(st)) e.seed_time_limit = "empty, or 30m, 12h, 7d";
  const p = t.proxy_url.trim();
  if (p && !/^socks5h?:\/\/([^@/]+@)?[^/:]+:\d{1,5}\/?$/.test(p)) {
    e.proxy_url = "socks5://host:port or socks5h://host:port (user:pass@ allowed)";
  }
  if (/\s/.test(t.bind_interface)) e.bind_interface = "an interface name such as wg0, no spaces";
  trackers.forEach((u, i) => {
    if (!/^(https?|udp):\/\//.test(u)) e[`trackers.${i}`] = `${u}: use http://, https://, or udp://`;
  });
  peers.forEach((u, i) => {
    if (!/^.+:\d{1,5}$/.test(u)) e[`peers.${i}`] = `${u}: use host:port`;
  });
  t.alt_speed.window.forEach((w, i) => {
    if (!hhmm(w.start, false)) e[`alt_speed.window.${i}.start`] = "HH:MM";
    if (!hhmm(w.end, true)) e[`alt_speed.window.${i}.end`] = "HH:MM or 24:00";
    else if (w.start.trim() === w.end.trim()) e[`alt_speed.window.${i}.end`] = "must differ from the start";
  });
  return e;
}

export function lines(s: string): string[] {
  return s
    .split(/\r?\n/)
    .map((x) => x.trim())
    .filter((x) => x.length > 0);
}
