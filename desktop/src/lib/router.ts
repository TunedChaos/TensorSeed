// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Hash routes inside the window. Deep links (tensorseed://...) are parsed
// natively and arrive as a Route, which maps onto one of these:
//
//   #/                                       catalogs
//   #/local                                  local copies
//   #/settings                               torrent settings
//   #/entry/<model|dataset>/<org>/<name>?revision=<40 hex>&catalog=<name>

import type { RepoType, Route } from "./native";

export type View =
  | { kind: "catalogs" }
  | { kind: "local" }
  | { kind: "settings" }
  | { kind: "entry"; repoType: RepoType; id: string; revision: string | null; catalog: string | null }
  | { kind: "notFound"; hash: string };

const PART = /^[A-Za-z0-9._-]+$/;
const COMMIT = /^[0-9a-f]{40}$/;
const CATALOG = /^[a-z0-9-]{1,64}$/;

export function parseHash(hash: string): View {
  const raw = hash.replace(/^#/, "");
  const [path, query = ""] = raw.split("?", 2);
  const parts = path.split("/").filter((p) => p.length > 0);
  if (parts.length === 0 || (parts.length === 1 && parts[0] === "catalogs")) return { kind: "catalogs" };
  if (parts.length === 1 && parts[0] === "local") return { kind: "local" };
  if (parts.length === 1 && parts[0] === "settings") return { kind: "settings" };
  if (parts[0] === "entry" && parts.length === 4 && (parts[1] === "model" || parts[1] === "dataset")) {
    const [, repoType, org, name] = parts;
    if (PART.test(org) && PART.test(name) && org !== "." && org !== ".." && name !== "." && name !== "..") {
      const q = new URLSearchParams(query);
      const rev = q.get("revision")?.toLowerCase() ?? null;
      const cat = q.get("catalog");
      return {
        kind: "entry",
        repoType: repoType as RepoType,
        id: `${org}/${name}`,
        revision: rev && COMMIT.test(rev) ? rev : null,
        catalog: cat && CATALOG.test(cat) ? cat : null,
      };
    }
  }
  return { kind: "notFound", hash: raw };
}

export function entryHash(repoType: RepoType, id: string, revision?: string | null, catalog?: string | null): string {
  const q = new URLSearchParams();
  if (revision) q.set("revision", revision);
  if (catalog) q.set("catalog", catalog);
  const qs = q.toString();
  return `#/entry/${repoType}/${id}${qs ? `?${qs}` : ""}`;
}

export function routeHash(r: Route): string {
  switch (r.kind) {
    case "catalogs":
      return "#/";
    case "settings":
      return "#/settings";
    case "entry":
      return entryHash(r.repoType, r.id, r.revision, r.catalog);
  }
}
