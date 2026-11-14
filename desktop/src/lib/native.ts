// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// The native bridge: every call is a Tauri command in src-tauri/src, so
// verification, settings, and seeding run in Rust, not here. The types
// mirror the Rust result structs (camelCase), except `TorrentSettings`,
// which is the `[torrent]` table itself (snake_case, as in the config).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CatalogEntries, CatalogSource, CatalogSummary, ChainView } from "@tensorseed/ui/source";
import type { CommercialAssessment, Manifest, ThemePreference } from "@tensorseed/ui/types";

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

export type RepoType = "model" | "dataset";

/** `tensorseed_app::links::Route`. */
export type Route =
  | { kind: "catalogs" }
  | { kind: "settings" }
  | { kind: "entry"; repoType: RepoType; id: string; revision?: string | null; catalog?: string | null };

/** `DeepLink` in src-tauri/src/lib.rs. */
export type DeepLink = { url: string; route: Route | null; error: string | null };

/** Must match `DEEP_LINK_EVENT` in src-tauri/src/lib.rs. */
export function onDeepLink(handler: (link: DeepLink) => void): Promise<UnlistenFn> {
  return listen<DeepLink>("deep-link://open", (e) => handler(e.payload));
}

/** The link Desktop was started with, once. */
export function initialRoute(): Promise<DeepLink | null> {
  return invoke<DeepLink | null>("initial_route");
}

/** How a local copy compares with its catalog entry (`copies::CopyStatus`). */
export type CopyStatus = {
  kind: "commitMismatch" | "partial" | "unsigned" | "complete" | "unchecked";
  partial: boolean;
  unsigned: boolean;
  commitMismatch: boolean;
  presentFiles: number;
  presentBytes: number;
  catalogFiles: number | null;
  catalogCommit: string | null;
  missingFiles: number;
  summary: string;
  verifyDefault: "catalog" | "record";
  canComplete: boolean;
};

export type LocalCopy = {
  repoType: RepoType;
  id: string;
  dir: string;
  pulled: boolean;
  /** This copy's own commit, from its pull record. */
  commit: string | null;
  pulledAt: string | null;
  signed: boolean;
  source: string | null;
  /** Files and bytes this copy holds (skipped files not counted). */
  files: number;
  bytes: number;
  recordFiles: number;
  skipped: number;
  status: CopyStatus | null;
  hasTorrent: boolean;
  seeding: boolean;
  error: string | null;
};

export type EntryPage = {
  id: string;
  repoType: RepoType;
  catalog: string | null;
  catalogLabel: string | null;
  manifest: (Manifest & { published_at?: string; publisher_tool?: string; file_pages?: number }) | null;
  manifestSource: "catalog" | "local" | null;
  manifestError: string | null;
  signed: boolean;
  tombstone: { reason?: string; detail?: string; [k: string]: unknown } | null;
  takenDown: boolean;
  takenDownIn: string[];
  chain: ChainView | null;
  commercial: CommercialAssessment | null;
  synced: boolean;
  syncError: string | null;
  local: LocalCopy;
  cliFound: boolean;
};

export type EntryArgs = { repoType: RepoType; id: string; revision?: string | null; catalog?: string | null };

export function entryShow(a: EntryArgs, sync: boolean): Promise<EntryPage> {
  return invoke<EntryPage>("entry_show", {
    repoType: a.repoType,
    id: a.id,
    revision: a.revision ?? null,
    catalog: a.catalog ?? null,
    sync,
  });
}

export type LocalList = { downloadDir: string; copies: LocalCopy[] };

export function localList(): Promise<LocalList> {
  return invoke<LocalList>("local_list");
}

export type SpeedWindow = { days: string[]; start: string; end: string };

/** `[torrent]` (`tensorseed_app::config::Torrent`), snake_case as in the file. */
export type TorrentSettings = {
  create: boolean;
  hybrid: boolean;
  piece_length: number;
  trackers: string[];
  listen_addr: string;
  listen_port: number;
  upload_limit_kbps: number;
  dht: boolean;
  lsd: boolean;
  peers: string[];
  pull_swarm: boolean;
  swarm_no_peer_secs: number;
  swarm_stall_secs: number;
  swarm_timeout_secs: number;
  download_limit_kbps: number;
  max_peers_per_torrent: number;
  utp: boolean;
  proxy_url: string;
  bind_interface: string;
  concurrent_init_limit: number;
  torrent_upload_limit_kbps: number;
  torrent_download_limit_kbps: number;
  seed_ratio_limit: number;
  seed_time_limit: string;
  seed_limit_action: "pause" | "remove";
  alt_speed: {
    enabled: boolean;
    upload_limit_kbps: number;
    download_limit_kbps: number;
    window: SpeedWindow[];
  };
};

export type SettingsView = {
  configPath: string;
  exists: boolean;
  torrent: TorrentSettings;
  defaults: TorrentSettings;
  envOverrides: string[];
  bindInterfaceSupported: boolean;
};

export type SettingsSaved = { configPath: string; appliedToSeeder: boolean; envOverrides: string[] };

export function settingsGet(): Promise<SettingsView> {
  return invoke<SettingsView>("settings_get");
}

export function settingsCheck(torrent: TorrentSettings): Promise<void> {
  return invoke<void>("settings_check", { torrent });
}

export function settingsSet(torrent: TorrentSettings): Promise<SettingsSaved> {
  return invoke<SettingsSaved>("settings_set", { torrent });
}

/** `pulls::Progress` in src-tauri/src/pulls.rs. */
export type PullProgress = {
  phase: "resolving" | "swarm" | "downloading";
  filesTotal: number;
  filesDone: number;
  bytesTotal: number;
  bytesDone: number;
  /** Network bytes per second. */
  rate: number;
  current: string[];
  /** Connected peers in the swarm step. */
  peers: number;
};

export type JobState = "running" | "ok" | "failed" | "paused" | "cancelled";

export type JobEvent = {
  job: number;
  kind: "pull" | "verify";
  repoType: RepoType;
  id: string;
  state: JobState;
  line: string | null;
  message: string | null;
  progress: PullProgress | null;
  out: string | null;
  /** Id in the job database shared with `tensorseed jobs`. */
  dbJob: string | null;
};

export function onJob(handler: (e: JobEvent) => void): Promise<UnlistenFn> {
  return listen<JobEvent>("job://event", (e) => handler(e.payload));
}

/** `include` and `exclude` select files by path (`*` and `?` globs). */
export function pullStart(a: EntryArgs, sel: { include?: string[]; exclude?: string[] } = {}): Promise<number> {
  return invoke<number>("pull_start", {
    repoType: a.repoType,
    id: a.id,
    revision: a.revision ?? null,
    catalog: a.catalog ?? null,
    include: sel.include ?? null,
    exclude: sel.exclude ?? null,
  });
}

/** Stop a running pull and keep its partial downloads. */
export function pullPause(job: number): Promise<void> {
  return invoke<void>("pull_pause", { job });
}

/** Stop a running pull and delete its partial downloads (finished files stay). */
export function pullCancel(job: number): Promise<void> {
  return invoke<void>("pull_cancel", { job });
}

/** `pulls::JobRow`: one job of the database shared with the CLI. */
export type JobRow = {
  id: string;
  label: string;
  outDir: string;
  status: "pending" | "running" | "paused" | "done" | "failed" | "interrupted" | string;
  filesTotal: number;
  filesDone: number;
  error: string | null;
  updatedUnix: number;
  busy: boolean;
  windowJob: number | null;
  resumable: boolean;
};

export type JobsList = { active: JobEvent[]; history: JobRow[]; error: string | null };

export function jobsList(): Promise<JobsList> {
  return invoke<JobsList>("jobs_list");
}

/** Resume an unfinished pull, started here or by `tensorseed pull`. */
export function jobResume(id: string): Promise<number> {
  return invoke<number>("job_resume", { id });
}

/** Delete an unfinished pull's partial downloads and forget it. */
export function jobDiscard(id: string): Promise<void> {
  return invoke<void>("job_discard", { id });
}

export function copyOpen(repoType: RepoType, id: string): Promise<void> {
  return invoke<void>("copy_open", { repoType, id });
}

/** Delete a local copy from disk. Refused while it seeds or a pull writes to it. */
export function copyDelete(repoType: RepoType, id: string): Promise<string> {
  return invoke<string>("copy_delete", { repoType, id });
}

/** Pause every seed and pull (the tray's Pause all). */
export function allPause(): Promise<number> {
  return invoke<number>("all_pause");
}

export function allResume(): Promise<number> {
  return invoke<number>("all_resume");
}

/** `auto` checks a complete signed copy against the catalog and any other copy against its own pull record. */
export type VerifyMode = "auto" | "record" | "catalog";

export function verifyStart(a: EntryArgs, mode: VerifyMode = "auto"): Promise<number> {
  return invoke<number>("verify_start", { repoType: a.repoType, id: a.id, catalog: a.catalog ?? null, mode });
}

export function seedStart(a: EntryArgs): Promise<string> {
  return invoke<string>("seed_start", { repoType: a.repoType, id: a.id });
}

export function seedsPause(): Promise<number> {
  return invoke<number>("seeds_pause");
}

export function seedsResume(): Promise<number> {
  return invoke<number>("seeds_resume");
}

export function seedSetPaused(name: string, pause: boolean): Promise<string> {
  return invoke<string>("seed_set_paused", { name, pause });
}

export function seedStop(name: string): Promise<string> {
  return invoke<string>("seed_stop", { name });
}

/** Pause or resume one revision, or all (`name` null), in the attached serve. */
export function serveSetPaused(name: string | null, pause: boolean): Promise<number> {
  return invoke<number>("serve_set_paused", { name, pause });
}

export type SeedView = {
  name: string;
  dir: string;
  bytes: number;
  uploaded: number;
  peers: number;
  state: "seeding" | "paused" | "stopped" | "error";
  detail: string | null;
};

export type ServeTorrent = { name: string; state: string; bytes: number; uploaded: number; peers: number };

export type ServeView = {
  kind: "serve" | "seed";
  pid: number | null;
  /** Attached over the local control endpoint, so it can be paused here. */
  control: boolean;
  torrents: number;
  paused: number;
  uploaded: number;
  list: ServeTorrent[];
  uploadMibps: number;
  downloadMibps: number;
  peers: number;
  status: string;
};

export type Status = {
  desktop: {
    running: boolean;
    listen: string | null;
    seeding: number;
    paused: number;
    stopped: number;
    uploadMibps: number;
    downloadMibps: number;
    peers: number;
    uploaded: number;
    torrents: SeedView[];
  };
  /** Another seeder on this machine (`tensorseed serve` or `tensorseed seed`). */
  serve: ServeView | null;
  jobs: JobEvent[];
  downloading: number;
  pausedPulls: number;
};

export function status(): Promise<Status> {
  return invoke<Status>("status");
}

export function onStatus(handler: (s: Status) => void): Promise<UnlistenFn> {
  return listen<Status>("status://update", (e) => handler(e.payload));
}

const THEME_KEY = "tensorseed.desktop.theme";

export function loadTheme(): ThemePreference {
  const t = localStorage.getItem(THEME_KEY);
  return t === "light" || t === "dark" || t === "system" ? t : "system";
}

export function saveTheme(theme: ThemePreference): void {
  localStorage.setItem(THEME_KEY, theme);
}

/** `updater::Channel` in src-tauri/src/updater.rs. */
export type UpdateChannel = "app-image" | "nsis" | "msi" | "deb" | "rpm" | "flatpak" | "mac-app" | "dev";

/** `updater::Info`. */
export type UpdateInfo = {
  currentVersion: string;
  channel: UpdateChannel;
  selfUpdates: boolean;
  elsewhere: string | null;
  checkOnStart: boolean;
};

/** `updater::Available`. */
export type UpdateAvailable = { currentVersion: string; version: string; notes: string; date: string | null };

/** `updater::Progress`. */
export type UpdateProgress = { downloaded: number; total: number | null; done: boolean };

export function updateInfo(): Promise<UpdateInfo> {
  return invoke<UpdateInfo>("update_info");
}

/** Fetch latest.json only; nothing is downloaded or installed. */
export function updateCheck(): Promise<UpdateAvailable | null> {
  return invoke<UpdateAvailable | null>("update_check");
}

/** Download, verify, install, and restart. Only after the user confirms. */
export function updateInstall(version: string): Promise<void> {
  return invoke<void>("update_install", { version });
}

export function updateSetCheckOnStart(enabled: boolean): Promise<void> {
  return invoke<void>("update_set_check_on_start", { enabled });
}

/** Must match `UPDATE_AVAILABLE_EVENT` in src-tauri/src/updater.rs. */
export function onUpdateAvailable(handler: (u: UpdateAvailable) => void): Promise<UnlistenFn> {
  return listen<UpdateAvailable>("update://available", (e) => handler(e.payload));
}

/** Must match `UPDATE_PROGRESS_EVENT` in src-tauri/src/updater.rs. */
export function onUpdateProgress(handler: (p: UpdateProgress) => void): Promise<UnlistenFn> {
  return listen<UpdateProgress>("update://progress", (e) => handler(e.payload));
}
