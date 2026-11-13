import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// Tauri loads the built files from disk in production and this dev server
// in `pnpm tauri dev`. Nothing is fetched from the network at runtime.
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  base: "./",
  clearScreen: false,
  build: {
    // WebKitGTK 2.36+ (Ubuntu 22.04) on Linux, WebView2 on Windows.
    target: ["es2022", "chrome105", "safari15"],
    sourcemap: false,
    cssCodeSplit: true,
    assetsInlineLimit: 0,
  },
  server: {
    host: "localhost",
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
