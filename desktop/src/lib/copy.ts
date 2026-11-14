// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import type { CopyStatus } from "./native";

/** Badge for a local copy. Partial, unsigned, and other revision copies are warnings about this machine, never about the catalog entry. */
export function copyBadge(s: CopyStatus | null): { cls: string; text: string } | null {
  if (!s) return null;
  switch (s.kind) {
    case "complete":
      return { cls: "badge-success", text: "complete" };
    case "unchecked":
      return { cls: "badge-ghost", text: "signed" };
    case "unsigned":
      return { cls: "badge-warning", text: "unsigned" };
    case "partial":
      return { cls: "badge-warning", text: s.unsigned ? "partial, unsigned" : "partial" };
    case "commitMismatch":
      return { cls: "badge-warning", text: "other revision" };
  }
}

/** "1 file" or "N files". */
export function fileCount(n: number): string {
  return `${n} ${n === 1 ? "file" : "files"}`;
}
