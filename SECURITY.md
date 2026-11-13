# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub's private
vulnerability reporting: open the repository's **Security** tab and choose
**Report a vulnerability**. Do not open a public issue, pull request, or
discussion for a suspected vulnerability.

Useful details: affected version or commit, platform, configuration (with
secrets removed), steps to reproduce, and the impact you expect.

What to expect: an acknowledgement within a few days, an assessment and a
plan once the problem is reproduced, and credit in the advisory unless you
prefer otherwise. Please give us a reasonable time to ship a fix before
disclosing publicly; we aim to publish an advisory together with the fix.

## Scope

In scope: the `tensorseed` binary and crates in this repository, the signed
catalog format and its verification, the Chia chain check, the release
archives and their signatures, and the website and catalog hosting
configuration in `site/`.

Especially interesting: any way to make a client accept bytes, a manifest,
or a catalog head that the publisher key or the Chia anchor does not
vouch for; path traversal or writes outside the output directory; ways for
a peer, mirror, or webseed to do more than be slow or missing; and secrets
leaking into logs or files.

Out of scope: content of mirrored models (report those through the
takedown process in the catalog's `ts/v1/catalog` contact field), denial of
service by simply not serving data, and issues in third party services.

## Keys

The catalog publisher key and the release signing key are separate. See
[RELEASE-KEY.md](RELEASE-KEY.md) for their roles, how they are stored and
rotated, and how to verify a release.
