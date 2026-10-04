import { describe, expect, it } from "vitest";
import type { TaxonomyCategory } from "@/lib/types";
import { buildTree, formatDelta } from "./builders";

describe("spending category comparison labels", () => {
  const formatting = {
    formatPercent: (value: number | null | undefined) => `${Math.round((value ?? 0) * 100)}%`,
  };

  it.each([
    [-50, 120],
    [-50, -20],
  ])("does not report a percentage against net-credit baseline %s", (baseline, delta) => {
    expect(formatDelta(delta, baseline, formatting)).toBe("—");
  });

  it("keeps the direction and magnitude for positive prior spending", () => {
    expect(formatDelta(20, 100, formatting)).toBe("↑ 20%");
    expect(formatDelta(-120, 100, formatting)).toBe("↓ 120%");
    expect(formatDelta(0, 100, formatting)).toBe("—");
    expect(formatDelta(20, 0, formatting)).toBe("new");
    expect(formatDelta(-20, 0, formatting)).toBe("—");
  });
});

describe("spending category totals", () => {
  it("labels prior-only uncategorized activity without exposing the internal id", () => {
    const tree = buildTree({
      breakdown: [],
      priorBreakdown: [
        {
          categoryId: "__uncategorized__",
          taxonomyId: "spending_categories",
          amount: 20,
          count: 1,
        },
      ],
      budgetRows: [],
      taxonomyCategories: [],
      sort: "spent",
      compareNames: (a, b) => a.localeCompare(b),
      uncategorizedLabel: "Uncategorized",
    });
    expect(tree[0].name).toBe("Uncategorized");
    expect(tree[0].priorSpent).toBe(20);
  });
  it("keeps signed current and prior refunds in the hierarchy", () => {
    const categories = [
      { id: "home", name: "Home", parentId: null },
      { id: "rent", name: "Rent", parentId: "home" },
      { id: "utilities", name: "Utilities", parentId: "home" },
      { id: "refund", name: "Refund", parentId: null },
    ] as TaxonomyCategory[];
    const row = (categoryId: string, amount: number) => ({
      categoryId,
      amount,
      taxonomyId: "spending_categories",
      count: 1,
    });
    const tree = buildTree({
      breakdown: [row("rent", 100), row("utilities", -150), row("refund", -20)],
      priorBreakdown: [row("refund", -10)],
      budgetRows: [],
      taxonomyCategories: categories,
      sort: "spent",
      compareNames: (a, b) => a.localeCompare(b),
    });
    expect(tree.reduce((sum, node) => sum + node.spent, 0)).toBe(-70);
    expect(tree.reduce((sum, node) => sum + node.priorSpent, 0)).toBe(-10);
    const home = tree.find((node) => node.id === "home")!;
    expect(home.children.reduce((sum, node) => sum + node.spent, 0)).toBe(home.spent);
    expect(home.children.find((node) => node.id === "utilities")?.spent).toBe(-150);
  });
});
