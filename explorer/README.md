# Tensorseed explorer

A read-only catalog browser (spec section 8.4). It is a static
Svelte 5 + Tailwind CSS + DaisyUI app. Verification runs in WebAssembly
from `tensorseed-wasm`, which wraps `tensorseed-verify`. The page fetches
catalog files from the catalog host and coin records from a public
coinset-style API, then checks signatures and the Chia anchor locally.

Shared components, types, formatting helpers, and the theme live in the
`ui/` package (`@tensorseed/ui`, linked with `link:../ui`) so Tensorseed
Desktop can reuse them. Explorer only code (the WASM data layer, local
storage, the entry and catalogs pages) stays in `explorer/src`.

The official catalog pin (name, publisher key, store id) is compiled into
the page. You can add other catalogs; those settings stay in this browser.

## Build

Use pnpm, not npm.

```fish
# from the repo root, once
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked --version 0.2.130
pnpm --dir explorer install
pnpm --dir explorer build
```

The production output is `explorer/dist`. Deploy it to Cloudflare Pages as
`explore.tensorseed.dev` with `site/deploy.sh`.

There are no third-party scripts, fonts, or CDNs at runtime. Styles are
compiled at build time. `src/pkg/` is generated and not committed.
