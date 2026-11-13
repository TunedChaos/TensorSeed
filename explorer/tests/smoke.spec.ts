import { test, expect } from "@playwright/test";
import path from "node:path";

const shotDir = "/workspace";

test("official catalog verifies and lists entries", async ({ page }) => {
  await page.goto("./");
  await expect(page.getByRole("status")).toContainText(/Verified/i, { timeout: 60_000 });
  await expect(page.getByRole("link", { name: "openai/gsm8k" })).toBeVisible();
  await expect(page.getByRole("link", { name: "TunedChaos/ChaosNexus_Tuned_v1-GGUF" })).toBeVisible();
  await expect(page.locator('[data-commercial="allowed"]').first()).toBeVisible();
  await expect(page.locator('[data-commercial="unknown"]')).toHaveCount(1);
  await expect(page.getByText(/not legal advice/i).first()).toBeVisible();
  await page.screenshot({ path: path.join(shotDir, "explorer-list.png"), fullPage: true });

  await page.getByLabel("Filter by commercial serving").selectOption("commercial");
  await expect(page.getByRole("link", { name: "TunedChaos/ChaosNexus_Tuned_v1-GGUF" })).toHaveCount(0);
  await expect(page.getByRole("link", { name: "openai/gsm8k" })).toBeVisible();
  await page.getByLabel("Filter by commercial serving").selectOption("all");

  await page.getByRole("searchbox").fill("gsm8k");
  await expect(page.getByRole("link", { name: "openai/gsm8k" })).toBeVisible();
  await expect(page.getByRole("link", { name: "TinyLlama/TinyLlama-1.1B-Chat-v1.0" })).toHaveCount(0);
  await page.screenshot({ path: path.join(shotDir, "explorer-filter.png"), fullPage: true });

  await page.getByRole("link", { name: "openai/gsm8k" }).click();
  await expect(page.getByRole("heading", { name: "openai/gsm8k" })).toBeVisible();
  await expect(page.getByText("tensorseed pull openai/gsm8k --type dataset")).toBeVisible();
  await expect(page.getByText("Commercial serving:")).toBeVisible();
  await expect(page.getByRole("link", { name: "download .torrent" })).toBeVisible();
  await expect(page.getByRole("link", { name: "open magnet link" })).toBeVisible();
  await page.screenshot({ path: path.join(shotDir, "explorer-entry.png"), fullPage: true });

  await page.getByRole("button", { name: "Show details" }).click();
  await expect(page.getByText("Puzzle hash:")).toBeVisible();
  await page.screenshot({ path: path.join(shotDir, "explorer-verified-details.png"), fullPage: true });

  await page.getByRole("link", { name: "Catalogs" }).click();
  await expect(page.getByRole("heading", { name: "Catalogs" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Use another catalog" })).toBeVisible();
  await page.getByRole("button", { name: "Dark" }).click();
  await page.screenshot({ path: path.join(shotDir, "explorer-catalogs-dark.png"), fullPage: true });
});
