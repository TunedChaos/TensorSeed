// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Check every link in build/: internal paths must resolve the way Cloudflare
// Pages serves them (/x -> x.html, or x/index.html), and #anchors must exist
// on the target page. With --external, also fetch every external URL once.
//
//   node scripts/check-links.mjs [--external]

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname, relative } from "node:path";
import { fileURLToPath } from "node:url";

const build = join(dirname(fileURLToPath(import.meta.url)), "..", "build");
const external = process.argv.includes("--external");

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    // The catalog tree and mirror notice are copied in as is; skip them.
    if (p === join(build, "catalog") || p === join(build, "mirror")) continue;
    if (statSync(p).isDirectory()) out.push(...walk(p));
    else if (name.endsWith(".html")) out.push(p);
  }
  return out;
}

function fileFor(path) {
  const clean = decodeURIComponent(path.replace(/\/+$/, "")) || "";
  const candidates = clean === "" ? ["index.html"] : [clean.slice(1), `${clean.slice(1)}.html`, `${clean.slice(1)}/index.html`];
  return candidates.map((c) => join(build, c)).find((c) => existsSync(c) && statSync(c).isFile());
}

const idCache = new Map();
function ids(file) {
  if (!idCache.has(file)) {
    const html = readFileSync(file, "utf8");
    idCache.set(file, new Set([...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
  }
  return idCache.get(file);
}

const redirects = readFileSync(join(build, "_redirects"), "utf8")
  .split("\n")
  .filter((l) => l.trim() && !l.startsWith("#"))
  .map((l) => l.trim().split(/\s+/)[0]);

const pages = walk(build);
const problems = [];
const externals = new Map();

for (const page of pages) {
  const html = readFileSync(page, "utf8");
  const from = "/" + relative(build, page);
  for (const m of html.matchAll(/\s(?:href|src)="([^"]+)"/g)) {
    // Absolute links to this site (canonical, og:url) are checked as paths.
    const url = m[1].replace(/&amp;/g, "&").replace(/^https:\/\/tensorseed\.dev(?=\/)/, "");
    if (/^(mailto:|data:)/.test(url)) continue;
    if (/^https?:\/\//.test(url)) {
      if (!externals.has(url)) externals.set(url, from);
      continue;
    }
    const [path, hash] = url.split("#");
    let target = page;
    if (path) {
      if (!path.startsWith("/")) {
        problems.push(`${from}: relative link ${url}`);
        continue;
      }
      if (redirects.includes(path)) continue;
      target = fileFor(path.split("?")[0]);
      if (!target) {
        problems.push(`${from}: broken link ${url}`);
        continue;
      }
    }
    if (hash && target.endsWith(".html") && !ids(target).has(decodeURIComponent(hash))) {
      problems.push(`${from}: missing anchor ${url}`);
    }
  }
}

console.log(`checked ${pages.length} pages, ${externals.size} external URLs found`);

if (external) {
  const results = await Promise.all(
    [...externals.keys()].map(async (url) => {
      for (let attempt = 0; attempt < 3; attempt++) {
        try {
          const headers = { "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) tensorseed-site-link-check" };
          let res = await fetch(url, { method: "HEAD", redirect: "follow", headers });
          if (res.status === 405 || res.status === 403) res = await fetch(url, { redirect: "follow", headers });
          return [url, res.status];
        } catch (err) {
          if (attempt === 2) return [url, String(err)];
        }
      }
    }),
  );
  for (const [url, status] of results) {
    const ok = status === 200;
    console.log(`${ok ? "ok  " : "FAIL"} ${status} ${url}`);
    if (!ok) problems.push(`external ${status} ${url} (from ${externals.get(url)})`);
  }
}

if (problems.length) {
  console.error(problems.join("\n"));
  process.exit(1);
}
console.log("all links ok");
