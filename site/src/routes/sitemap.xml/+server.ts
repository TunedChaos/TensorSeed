// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import { GUIDES } from "#lib/guides.ts";
import { SITE_URL, STATIC_ROUTES } from "#lib/site.ts";

export const prerender = true;

export function GET(): Response {
  const paths = [...STATIC_ROUTES, ...GUIDES.map((g) => `/guides/${g.slug}`)];
  const body =
    `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n` +
    paths.map((p) => `  <url><loc>${SITE_URL}${p === "/" ? "/" : p}</loc></url>`).join("\n") +
    `\n</urlset>\n`;
  return new Response(body, { headers: { "Content-Type": "application/xml" } });
}
