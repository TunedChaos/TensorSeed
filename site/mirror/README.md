# mirror.tensorseed.dev landing pages

`mirror.tensorseed.dev` is the public R2 bucket `tensorseed-mirror` on an R2
custom domain. R2 has no index documents and no directory listing, so these
pages are uploaded as plain objects and served for the bare paths by a zone
URL Rewrite rule.

Objects:

- `index.html` (served for `/` and `/t/`)
- `404.html` (branded not found page, for a future custom error rule)

Upload (account R2 write access required):

    pnpm dlx wrangler r2 object put tensorseed-mirror/index.html --remote \
      --file site/mirror/index.html --content-type "text/html; charset=utf-8" \
      --cache-control "public, max-age=300"
    pnpm dlx wrangler r2 object put tensorseed-mirror/404.html --remote \
      --file site/mirror/404.html --content-type "text/html; charset=utf-8" \
      --cache-control "public, max-age=300"

Zone rule (Rules, Transform Rules, URL Rewrite Rules, tensorseed.dev):

- Name: mirror landing page
- When: `http.host eq "mirror.tensorseed.dev" and http.request.uri.path in {"/" "/t/"}`
- Path: rewrite to static `/index.html`; query: preserve

Every other path, including `.torrent` files, webseed file data, and Range
requests, goes straight to R2 unchanged.
