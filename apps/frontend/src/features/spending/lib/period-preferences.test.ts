import { describe, expect, it } from "vitest";

import {
  dashboardInsightHref,
  normalizeReportsPeriod,
  shouldPreferDashboardPeriod,
} from "./period-preferences";

describe("spending period preferences", () => {
  it("carries the visible dashboard period and range into each drilldown", () => {
    expect(dashboardInsightHref("where", "MTD", {}, "#cashflow")).toBe(
      "/spending/insights?stage=where&period=MTD#cashflow",
    );
    expect(dashboardInsightHref("changed", "3M")).toBe(
      "/spending/insights?stage=changed&period=3M",
    );
    expect(dashboardInsightHref("when", "LAST_MONTH", { month: "2025-02" })).toBe(
      "/spending/insights?stage=when&period=LAST_MONTH&spendingMonth=2025-02",
    );
    expect(dashboardInsightHref("where", "1Y", { from: "2025-02-01", to: "2025-04-30" })).toBe(
      "/spending/insights?stage=where&period=1Y&spendingFrom=2025-02-01&spendingTo=2025-04-30",
    );
  });
  it("normalizes legacy dashboard month periods for insights", () => {
    expect(normalizeReportsPeriod("3M")).toBe("3M");
    expect(normalizeReportsPeriod("LAST_MONTH")).toBe("LAST_MONTH");
    expect(normalizeReportsPeriod("1M")).toBeNull();
  });

  it("uses the dashboard period when no valid insight period is stored", () => {
    expect(
      shouldPreferDashboardPeriod({
        persistedInsightPeriod: null,
        dashboardUpdatedAt: "0",
        insightUpdatedAt: "100",
      }),
    ).toBe(true);
  });

  it("keeps a newer insight period when returning from the dashboard", () => {
    expect(
      shouldPreferDashboardPeriod({
        persistedInsightPeriod: "6M",
        dashboardUpdatedAt: "100",
        insightUpdatedAt: "200",
      }),
    ).toBe(false);
  });

  it("uses the dashboard period after the dashboard selector changes", () => {
    expect(
      shouldPreferDashboardPeriod({
        persistedInsightPeriod: "6M",
        dashboardUpdatedAt: "300",
        insightUpdatedAt: "200",
      }),
    ).toBe(true);
  });
});
