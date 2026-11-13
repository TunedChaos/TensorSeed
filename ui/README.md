# @tensorseed/ui

Svelte 5 components, TypeScript types, formatting helpers, and the theme
shared by the Tensorseed explorer (`explorer/`) and Tensorseed Desktop
(`desktop/`). See `docs/plans/0.3.0-desktop.md`, milestone M3.1.

It is not published. Each app links it with `"@tensorseed/ui": "link:../ui"`
and compiles it as source, so there is no build step here. Svelte, Tailwind
CSS, and daisyUI come from the app that imports it.

- `components/`: `EntryList`, `StatusBanner`, `CommandBox`, `ThemeToggle`.
  Components take data and callbacks as props and never touch storage or
  the network themselves.
- `types`: catalog, entry, manifest, and verification types.
- `format`: byte sizes, short hashes, URL joining, pull commands.
- `source`: the `CatalogSource` interface that hides whether verification
  runs in WebAssembly (explorer) or natively (Desktop).
- `theme.css`: base typography plus an `@source` line so Tailwind sees the
  classes used here. Import it after Tailwind and daisyUI:

```css
@import "tailwindcss";
@plugin "daisyui" {
  themes: light --default, dark --prefersdark;
}
@import "@tensorseed/ui/theme.css";
```

Copyright Tuned Chaos. AGPL-3.0-or-later.
