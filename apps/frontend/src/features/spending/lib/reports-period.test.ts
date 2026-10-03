import { afterEach, describe, expect, it, vi } from "vitest";

import { periodLabel, periodToReportsRange } from "./reports-period";

describe("reports periods", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("resolves LAST_MONTH to the previous full calendar month", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-10T12:00:00.000Z"));

    const range = periodToReportsRange("LAST_MONTH", "UTC");

    expect(range.start.toISOString()).toBe("2026-05-01T00:00:00.000Z");
    expect(range.end.toISOString()).toBe("2026-05-31T23:59:59.999Z");
    expect(range.days).toBe(31);
    expect(range.months).toBe(1);
    expect(periodLabel("LAST_MONTH")).toBe("Last month");
  });

  it.each([
    ["3M", "2026-07-03T07:00:00.000Z"],
    ["6M", "2026-04-03T07:00:00.000Z"],
    ["1Y", "2025-10-03T07:00:00.000Z"],
  ] as const)("uses the dashboard's rolling window for %s", (period, start) => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-03T09:00:00Z"));
    const range = periodToReportsRange(period, "America/Los_Angeles");
    expect(range.start.toISOString()).toBe(start);
    expect(range.end.toISOString()).toBe("2026-10-04T06:59:59.999Z");
  });

  it("clamps the prior-year boundary on leap day", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2024-02-29T12:00:00Z"));
    expect(periodToReportsRange("1Y", "UTC").start.toISOString()).toBe("2023-02-28T00:00:00.000Z");
  });
});
