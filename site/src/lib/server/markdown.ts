// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import { Marked, type Tokens } from "marked";
import GithubSlugger from "github-slugger";
import { GUIDES } from "#lib/guides.ts";
import { REPO_URL } from "#lib/release.ts";

const sources = import.meta.glob("../../../../docs/*.md", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

export function readDoc(file: string): string {
  const key = Object.keys(sources).find((k) => k.endsWith(`/docs/${file}`));
  if (!key) throw new Error(`docs/${file} not found`);
  return sources[key];
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Resolve `a/b/../c` style paths relative to the repo root. */
function normalize(path: string): string {
  const out: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") out.pop();
    else out.push(part);
  }
  return out.join("/");
}

/**
 * Rewrite a link found in docs/<file>. Links to another guide become site
 * routes; other repo paths go to GitHub; absolute URLs and anchors stay.
 */
export function rewriteHref(href: string): string {
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("#") || href.startsWith("//")) return href;
  const [pathPart, hash] = href.split("#", 2);
  const anchor = hash ? `#${hash}` : "";
  const repoPath = normalize(`docs/${pathPart}`);
  const guide = GUIDES.find((g) => `docs/${g.file}` === repoPath);
  if (guide) return `/guides/${guide.slug}${anchor}`;
  const isDir = pathPart.endsWith("/") || repoPath === "";
  return `${REPO_URL}/${isDir ? "tree" : "blob"}/main/${repoPath}${anchor}`;
}

export type Heading = { depth: number; text: string; id: string };

export type RenderedDoc = { title: string; html: string; headings: Heading[] };

export function renderDoc(file: string): RenderedDoc {
  const source = readDoc(file);
  const slugger = new GithubSlugger();
  const headings: Heading[] = [];
  let title = "";

  const marked = new Marked({ gfm: true });
  marked.use({
    renderer: {
      heading(this: { parser: { parseInline(t: Tokens.Generic[]): string } }, token: Tokens.Heading) {
        const inner = this.parser.parseInline(token.tokens);
        const plain = token.text.replace(/`/g, "").replace(/\[([^\]]*)\]\([^)]*\)/g, "$1");
        const id = slugger.slug(plain);
        if (token.depth === 1 && !title) {
          title = plain;
          return `<h1 id="${id}">${inner}</h1>\n`;
        }
        if (token.depth <= 3) headings.push({ depth: token.depth, text: plain, id });
        return `<h${token.depth} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true" tabindex="-1">#</a>${inner}</h${token.depth}>\n`;
      },
      link(this: { parser: { parseInline(t: Tokens.Generic[]): string } }, token: Tokens.Link) {
        const href = rewriteHref(token.href);
        let text = this.parser.parseInline(token.tokens);
        // "[SELL-INFERENCE.md](SELL-INFERENCE.md)" reads better as the title.
        const guide = GUIDES.find((g) => href.startsWith(`/guides/${g.slug}`));
        if (guide && /^(<code>)?(docs\/)?[A-Z-]+\.md(<\/code>)?$/.test(text)) text = guide.title;
        const titleAttr = token.title ? ` title="${escapeHtml(token.title)}"` : "";
        return `<a href="${escapeHtml(href)}"${titleAttr}>${text}</a>`;
      },
      table(this: unknown, token: Tokens.Table) {
        // Wrap tables so wide ones scroll on small screens.
        const head = token.header
          .map((cell, i) => `<th${align(token.align[i])}>${inline(this, cell.tokens)}</th>`)
          .join("");
        const rows = token.rows
          .map((row) => `<tr>${row.map((cell, i) => `<td${align(token.align[i])}>${inline(this, cell.tokens)}</td>`).join("")}</tr>`)
          .join("\n");
        return `<div class="table-wrap" tabindex="0"><table class="table table-sm"><thead><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>\n`;
      },
      code(token: Tokens.Code) {
        const lang = (token.lang || "").split(/\s/)[0];
        const cls = lang ? ` class="language-${escapeHtml(lang)}"` : "";
        return `<pre tabindex="0"><code${cls}>${escapeHtml(token.text)}</code></pre>\n`;
      },
      html(token: Tokens.HTML | Tokens.Tag) {
        // The docs are plain Markdown. Drop comments (the SPDX header) and
        // show any other raw HTML as text.
        if (/^\s*<!--[\s\S]*?-->\s*$/.test(token.text)) return "";
        return escapeHtml(token.text);
      },
    },
  });

  const html = marked.parse(source, { async: false }) as string;
  return { title, html, headings };
}

function align(a: string | null): string {
  return a ? ` style="text-align:${a}"` : "";
}

function inline(self: unknown, tokens: Tokens.Generic[]): string {
  return (self as { parser: { parseInline(t: Tokens.Generic[]): string } }).parser.parseInline(tokens);
}
