import { render, screen } from "@/test/render";
import { createFormatter } from "@wealthfolio/ui";
import { createInstance } from "i18next";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import messages from "@/i18n/locales/en/spending.json";
import { computePace } from "./where-i-am-stage";

const i18n = createInstance();
await i18n.init({ lng: "en", resources: { en: { spending: messages } } });
const range = {
  start: new Date("2030-07-01T00:00:00Z"),
  end: new Date("2030-07-31T23:59:59Z"),
  days: 31,
  months: 1,
};

function pace(elapsed: number, remaining: number) {
  return computePace(range, 1200, 3000, "USD", false, i18n.t, createFormatter("en-US", "UTC"), {
    dailyAvg: 100,
    daysElapsed: elapsed,
    daysRemaining: remaining,
    projectedSpend: 1200 + 100 * remaining,
    expectedSpendToDate: (3000 * elapsed) / 31,
  });
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("spending pace forecast coverage", () => {
  it("does not present early-month spending as a full-month forecast", () => {
    vi.setSystemTime(new Date("2030-07-03T12:00:00Z"));
    const result = pace(3, 28);
    expect(result.projection).toBeNull();
    render(<>{result.narrative}</>);
    expect(screen.getByText(messages.whereIAm.forecastPending)).toBeInTheDocument();
    expect(screen.queryByText(/Projected/)).not.toBeInTheDocument();
  });

  it("uses the reconciled forecast once sufficient days have elapsed", () => {
    vi.setSystemTime(new Date("2030-07-08T12:00:00Z"));
    const result = pace(8, 23);
    expect(result.projection).toBe(3500);
    render(<>{result.narrative}</>);
    expect(screen.getByText(/Projected/)).toBeInTheDocument();
    expect(screen.queryByText(messages.whereIAm.forecastPending)).not.toBeInTheDocument();
  });

  it("reports actual spending for a closed period", () => {
    vi.setSystemTime(new Date("2030-08-02T12:00:00Z"));
    const result = pace(31, 0);
    expect(result.projection).toBe(1200);
    render(<>{result.narrative}</>);
    expect(screen.queryByText(/Projected/)).not.toBeInTheDocument();
  });
});
