// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// The guides shown on the site. Each one is rendered at build time from the
// Markdown file in the repo's docs/ folder, so there is a single copy.

export type GuideMeta = {
  slug: string;
  file: string;
  title: string;
  summary: string;
};

export const GUIDES: GuideMeta[] = [
  {
    slug: "run-a-mirror",
    file: "RUN-A-MIRROR.md",
    title: "Run a mirror",
    summary: "Seed the catalog and serve its bytes over HTTP, as a volunteer or a provider.",
  },
  {
    slug: "publish-a-catalog",
    file: "PUBLISH-A-CATALOG.md",
    title: "Publish a catalog",
    summary: "Sign and anchor your own catalog of models and datasets.",
  },
  {
    slug: "sell-inference",
    file: "SELL-INFERENCE.md",
    title: "Sell inference",
    summary: "Turn a verified copy into a signed, OpenAI compatible endpoint.",
  },
  {
    slug: "integrate-a-marketplace",
    file: "INTEGRATE-A-MARKETPLACE.md",
    title: "Integrate a marketplace",
    summary: "Check catalogs, seller claims, and signed responses as a market or gateway.",
  },
];

export function guideBySlug(slug: string): GuideMeta | undefined {
  return GUIDES.find((g) => g.slug === slug);
}
