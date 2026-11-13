#!/usr/bin/env bash
# Deploy the Tensorseed Pages sites.
#
# Required environment:
#   CLOUDFLARE_API_TOKEN    Account Cloudflare Pages Edit, and Zone DNS Edit
#                           for tensorseed.dev when attaching custom domains.
#   CLOUDFLARE_ACCOUNT_ID   defaults to the Tuned Chaos account below.
#
# This script never prints the token. It only checks that the variable is set.
#
# Two free Pages projects:
#   tensorseed          site/                 -> tensorseed.dev
#                                               tensorseed.pages.dev
#   tensorseed-catalog  site/catalog          -> catalog.tensorseed.dev
#                                               tensorseed-catalog.pages.dev
#
# The second project serves the SPEC 6.8 tree at the origin of
# catalog.tensorseed.dev (file.base_url), rather than under a /catalog path.
# Custom domains are attached once with the Pages domains API (see below).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ACCOUNT_ID="${CLOUDFLARE_ACCOUNT_ID:-REDACTED_ACCOUNT_ID}"
export CLOUDFLARE_ACCOUNT_ID="${ACCOUNT_ID}"
# Outside a Node project wrangler may resolve its cache to /node_modules/.cache
# and fail with EACCES after uploading; keep the cache somewhere writable.
export WRANGLER_CACHE_DIR="${WRANGLER_CACHE_DIR:-${TMPDIR:-/tmp}/wrangler-cache}"

if [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  echo "CLOUDFLARE_API_TOKEN is not set" >&2
  exit 1
fi
echo "CLOUDFLARE_API_TOKEN is set (length ${#CLOUDFLARE_API_TOKEN})"
echo "CLOUDFLARE_ACCOUNT_ID is set (length ${#CLOUDFLARE_ACCOUNT_ID})"

cd "${ROOT}"

echo "Deploying tensorseed from site/"
pnpm dlx wrangler pages deploy site --project-name tensorseed --branch main

echo "Deploying tensorseed-catalog from site/catalog"
pnpm dlx wrangler pages deploy site/catalog --project-name tensorseed-catalog --branch main

cat <<'EOF'

If you still need to attach the custom domains (once per project):

  curl -sS -X POST \
    "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/pages/projects/tensorseed/domains" \
    -H "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"name":"tensorseed.dev"}'

  curl -sS -X POST \
    "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/pages/projects/tensorseed-catalog/domains" \
    -H "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"name":"catalog.tensorseed.dev"}'

If Cloudflare does not create the DNS records, add proxied CNAMEs:

  tensorseed.dev          CNAME  tensorseed.pages.dev
  catalog.tensorseed.dev  CNAME  tensorseed-catalog.pages.dev

Create the projects first, if they do not exist:

  pnpm dlx wrangler pages project create tensorseed --production-branch main
  pnpm dlx wrangler pages project create tensorseed-catalog --production-branch main

EOF
