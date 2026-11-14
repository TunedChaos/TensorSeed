// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Finish build/ for the tensorseed Pages project: copy in the static trees
// that are not part of the SvelteKit app (the catalog mirror under /catalog/
// and the mirror notice under /mirror/), then check the result.

import { cpSync, existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const site = join(dirname(fileURLToPath(import.meta.url)), "..");
const build = join(site, "build");

for (const dir of ["catalog", "mirror"]) {
  cpSync(join(site, dir), join(build, dir), { recursive: true });
}

const required = [
  "index.html",
  "404.html",
  "download.html",
  "desktop.html",
  "serve.html",
  "guides.html",
  "guides/run-a-mirror.html",
  "guides/publish-a-catalog.html",
  "guides/sell-inference.html",
  "guides/integrate-a-marketplace.html",
  "verify.html",
  "support.html",
  "sitemap.xml",
  "robots.txt",
  "_headers",
  "_redirects",
  "favicon.svg",
  "xch-donate.svg",
  "og.png",
  "catalog/tensorseed/head.json",
  "mirror/index.html",
];
const missing = required.filter((f) => !existsSync(join(build, f)));
if (missing.length) {
  console.error(`build/ is missing: ${missing.join(", ")}`);
  process.exit(1);
}

const address = readFileSync(join(site, "xch-donate-address.txt"), "utf8").trim();
if (!readFileSync(join(build, "support.html"), "utf8").includes(address)) {
  console.error("build/support.html is missing the XCH address from site/xch-donate-address.txt");
  process.exit(1);
}

console.log("build/ is ready for the tensorseed Pages project");
