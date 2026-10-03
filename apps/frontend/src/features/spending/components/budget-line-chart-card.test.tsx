import { describe, expect, it } from "vitest";
import type { DayBucket } from "../types/report";
import { buildHistoricalPaceCurve } from "./budget-line-chart-card";

const day = (date: string, outflow: number): DayBucket => ({
  date,
  outflow,
  income: 0,
});

describe("monthly budget forecast history", () => {
  it("adds only spending after the same day, without averaging early rent across the month", () => {
    const history = [
      day("2030-08-01", 2000),
      day("2030-08-15", 700),
      day("2030-09-01", 2000),
      day("2030-09-15", 900),
    ];
    const curve = buildHistoricalPaceCurve(history, 31)!;
    expect(2000 + curve.remainingByDay[3]).toBe(2800);
    expect(curve.remainingByDay[15]).toBe(0);
    expect(curve.remainingByDay[31]).toBe(0);
    expect(curve.pctByDay[31]).toBe(1);
  });

  it("preserves refunds in the remaining net spending estimate", () => {
    const history = [
      day("2030-08-01", 2000),
      day("2030-08-10", -100),
      day("2030-08-15", 200),
      day("2030-09-01", 2000),
      day("2030-09-10", -100),
      day("2030-09-15", 200),
    ];
    const curve = buildHistoricalPaceCurve(history, 31)!;
    expect(curve.remainingByDay[3]).toBe(100);
    expect(curve.remainingByDay[10]).toBe(200);
  });

  it("ends a shorter current month at actual spending", () => {
    const curve = buildHistoricalPaceCurve(
      [
        day("2030-08-01", 2000),
        day("2030-08-31", 100),
        day("2030-09-01", 2000),
        day("2030-09-30", 100),
      ],
      28,
    )!;
    expect(curve.remainingByDay[28]).toBe(0);
    expect(curve.pctByDay[28]).toBe(1);
  });

  it("requires at least two recorded months rather than calling sparse history reliable", () => {
    expect(buildHistoricalPaceCurve([day("2030-09-01", 2000)], 31)).toBeNull();
    expect(buildHistoricalPaceCurve([], 31)).toBeNull();
  });
});
