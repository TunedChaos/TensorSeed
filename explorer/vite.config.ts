import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  // Relative paths so the same build can be opened from a local file or any static host.
  base: "./",
  build: {
    target: "es2022",
    sourcemap: true,
    cssCodeSplit: true,
    assetsInlineLimit: 0,
  },
  preview: {
    port: 4173,
    strictPort: true,
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
