// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Every page is prerendered to its own HTML file (/download -> download.html)
// and then hydrated, so navigation after the first load stays on the page.
export const prerender = true;
export const trailingSlash = "never";
