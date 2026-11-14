// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

export const SITE_URL = "https://tensorseed.dev";
export const SITE_NAME = "Tensorseed";
export const TAGLINE = "Download, seed, and serve AI models you can verify.";
export const SUBLINE = "Get the exact model. Prove you\u2019re serving it.";
export const DESCRIPTION =
  "Download, seed, and serve AI models you can verify. Tensorseed distributes Hugging Face models and datasets from a signed, Chia anchored catalog over BitTorrent v2 and HTTP or S3 mirrors, checks every byte, and can launch and vouch for an OpenAI compatible engine you install separately.";

export const CATALOG_URL = "https://catalog.tensorseed.dev/";
export const EXPLORER_URL = "https://explore.tensorseed.dev";

/** Catalog publisher key (minisign). Not the release key. */
export const PUBLISHER_KEY_ID = "CE1E0E3D543DFFB8";
export const PUBLISHER_PUBKEY = "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd";
export const CHIA_STORE_ID = "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76";

export type NavItem = { href: string; label: string };

export const NAV: NavItem[] = [
  { href: "/download", label: "Download" },
  { href: "/desktop", label: "Desktop" },
  { href: "/serve", label: "Serve" },
  { href: "/guides", label: "Guides" },
  { href: "/verify", label: "Verify" },
  { href: "/support", label: "Support" },
];

/** Old one page anchors and where they live now. */
export const HASH_REDIRECTS: Record<string, string> = {
  download: "/download",
  desktop: "/download#desktop",
  cli: "/download#cli",
  inference: "/serve",
  "verify-release": "/verify#release",
  support: "/support",
  "support-thanks": "/support#thanks",
};

/** Routes listed in sitemap.xml, besides the guides. */
export const STATIC_ROUTES = ["/", "/download", "/desktop", "/serve", "/guides", "/verify", "/support"];
