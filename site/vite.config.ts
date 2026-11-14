// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import adapter from "@sveltejs/adapter-static";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [
    tailwindcss(),
    sveltekit({
      compilerOptions: { runes: true },
      // Every route is prerendered to its own HTML file. There is no SPA
      // fallback: an unknown path gets the prerendered 404.html with a real
      // 404 status from Cloudflare Pages.
      adapter: adapter({ pages: "build", assets: "build", fallback: undefined, strict: true }),
      // Absolute asset paths, so 404.html works at any depth.
      paths: { relative: false },
      prerender: {
        handleHttpError: "fail",
        handleMissingId: "fail",
        handleUnseenRoutes: "fail",
      },
    }),
  ],
  server: {
    // The guides are read from ../docs at build time.
    fs: { allow: [".."] },
    port: 5174,
    strictPort: true,
  },
  build: {
    target: "es2022",
  },
});
