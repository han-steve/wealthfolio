import { describe, expect, it } from "vitest";
import type { NetWorthHistoryPoint } from "@/lib/types";
import { transformData } from "./net-worth-chart";

const point = (date: string, value: string): NetWorthHistoryPoint => ({
  date,
  netWorth: value,
  totalAssets: value,
  totalLiabilities: "0",
  currency: "USD",
  portfolioValue: value,
  alternativeAssetsValue: "0",
  netContribution: "0",
  breakdown: {},
});

describe("net worth chart missing values", () => {
  it("preserves missing dates as gaps rather than false zero balances", () => {
    const data = transformData([point("2024-03-09", "100"), point("2024-03-12", "120")]);
    expect(data.map((p) => [p.date, p.netWorth])).toEqual([
      ["2024-03-09", 100],
      ["2024-03-10", null],
      ["2024-03-11", null],
      ["2024-03-12", 120],
    ]);
  });
  it("distinguishes a real zero from invalid numbers", () => {
    expect(
      transformData([point("2024-01-01", "0"), point("2024-01-02", "NaN")]).map((p) => p.netWorth),
    ).toEqual([0, null]);
  });
});
