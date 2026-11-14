// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import { error } from "@sveltejs/kit";
import { GUIDES, guideBySlug } from "#lib/guides.ts";
import { renderDoc } from "#lib/server/markdown.ts";
import { REPO_URL } from "#lib/release.ts";
import type { EntryGenerator, PageServerLoad } from "./$types";

export const entries: EntryGenerator = () => GUIDES.map((g) => ({ slug: g.slug }));

export const load: PageServerLoad = ({ params }) => {
  const guide = guideBySlug(params.slug);
  if (!guide) error(404, "Guide not found");
  const doc = renderDoc(guide.file);
  return {
    guide,
    ...doc,
    sourceUrl: `${REPO_URL}/blob/main/docs/${guide.file}`,
  };
};
