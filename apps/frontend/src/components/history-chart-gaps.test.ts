import { describe, expect, it } from "vitest";
import { hasDailyHistoryGaps, withDailyHistoryGaps } from "./history-chart-gaps";

const point = (date: string) => ({ date, currency: "USD", totalValue: 150, netContribution: 100 });

describe("daily valuation gaps", () => {
  it("preserves real values and inserts unknown days, never zero balances", () => {
    const data = [point("2026-07-23"), point("2026-08-07")];
    const plotted = withDailyHistoryGaps(data);
    expect(plotted).toHaveLength(16);
    expect(plotted[0]).toEqual(data[0]);
    expect(plotted.at(-1)).toEqual(data[1]);
    expect(
      plotted.slice(1, -1).every((row) => row.totalValue === null && row.netContribution === null),
    ).toBe(true);
    expect(hasDailyHistoryGaps(data)).toBe(true);
    expect(data).toHaveLength(2);
  });

  it("does not invent leading or trailing dates", () => {
    const data = [point("2026-09-02"), point("2026-09-03")];
    expect(withDailyHistoryGaps(data)).toEqual(data);
    expect(hasDailyHistoryGaps(data)).toBe(false);
    expect(withDailyHistoryGaps([])).toEqual([]);
    expect(withDailyHistoryGaps([data[0]])).toEqual([data[0]]);
  });

  it("keeps calendar days across daylight-saving transitions", () => {
    const plotted = withDailyHistoryGaps([point("2026-03-07"), point("2026-03-10")]);
    expect(plotted.map((row) => row.date)).toEqual([
      "2026-03-07",
      "2026-03-08",
      "2026-03-09",
      "2026-03-10",
    ]);
  });
});
