import { performanceSummaryReturn } from "@/lib/performance";
import type { AccountValuation, PerformanceResult } from "@/lib/types";
import { render, screen } from "@/test/render";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  accountLifetimeValues,
  accountPerformanceAmount,
  accountPerformanceReasons,
} from "./account-performance-quality";
import { AccountPerformanceWarnings } from "./account-performance-warnings";

const privacy = vi.hoisted(() => ({ hidden: false }));
vi.mock("@wealthfolio/ui", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@wealthfolio/ui")>()),
  useBalancePrivacy: () => ({ isBalanceHidden: privacy.hidden }),
}));

const incomplete =
  "Performance attribution is incomplete for this period. Difference: 50; tolerance: 1. Review Health Center for possible data issues.";

function performance(): PerformanceResult {
  return {
    scope: { id: "synthetic-account", currency: "USD" },
    period: { startDate: "2025-01-01", endDate: "2026-01-01" },
    mode: "timeWeighted",
    returns: { twr: 0.1 },
    attribution: {
      contributions: 0,
      distributions: 0,
      income: 0,
      realizedPnl: 0,
      unrealizedPnlChange: 0,
      fxEffect: 0,
      fees: 0,
      taxes: 0,
      residual: 0,
    },
    risk: {},
    dataQuality: { status: "ok", warnings: [] },
    summary: {
      amount: 40,
      percent: 0.1,
      method: "timeWeighted",
      basis: "marketValue",
      quality: "ok",
      amountStatus: "complete",
      percentStatus: "complete",
      basisStatus: "notApplicable",
      reasons: [],
    },
    series: [],
  };
}

const valuation = {
  accountId: "synthetic-account",
  totalValue: 200,
  netContribution: 160,
} as AccountValuation;

describe("account performance quality", () => {
  beforeEach(() => {
    privacy.hidden = false;
  });

  it.each(["summary", "warnings", "notApplicableReasons"] as const)(
    "withholds unreconciled amounts from %s without suppressing valid TWR",
    (source) => {
      const result = performance();
      if (source === "summary") result.summary!.reasons = [incomplete];
      else result.dataQuality[source] = [incomplete];
      expect(accountPerformanceAmount(result)).toBeNull();
      expect(accountLifetimeValues(valuation, result)).toEqual({
        netContribution: null,
        returnAmount: null,
      });
      expect(performanceSummaryReturn(result)).toBe(0.1);
    },
  );

  it("recognizes the backend's legacy attribution diagnostic", () => {
    const result = performance();
    result.dataQuality.warnings = ["Attribution residual exceeds tolerance."];
    expect(accountPerformanceAmount(result)).toBeNull();
  });

  it("retains complete amounts with benign partial quality or unavailable IRR", () => {
    const result = performance();
    result.dataQuality = {
      status: "partial",
      warnings: ["Synthetic quote notice."],
      notApplicableReasons: ["IRR unavailable: no sign change."],
    };
    result.summary!.quality = "partial";
    result.attribution.residual = 0.01;
    expect(accountPerformanceAmount(result)).toBe(40);
    expect(accountLifetimeValues(valuation, result)).toEqual({
      netContribution: 160,
      returnAmount: 40,
    });
  });

  it("honors typed amount unavailability even when a number is present", () => {
    const result = performance();
    result.summary!.amountStatus = "unavailable";
    expect(accountLifetimeValues(valuation, result).returnAmount).toBeNull();
  });

  it.each(["noData", "notApplicable"] as const)(
    "does not synthesize lifetime values for %s",
    (status) => {
      const result = performance();
      result.dataQuality.status = status;
      expect(accountLifetimeValues(valuation, result).netContribution).toBeNull();
    },
  );

  it("does not substitute selected-period or another account's data for missing lifetime quality", () => {
    expect(accountLifetimeValues(valuation, undefined).returnAmount).toBeNull();
    const result = performance();
    result.scope.id = "another-account";
    expect(accountLifetimeValues(valuation, result).returnAmount).toBeNull();
  });

  it("preserves genuine zero contributions and rejects non-finite values", () => {
    expect(accountLifetimeValues({ ...valuation, netContribution: 0 }, performance())).toEqual({
      netContribution: 0,
      returnAmount: 200,
    });
    expect(
      accountLifetimeValues({ ...valuation, totalValue: NaN }, performance()).returnAmount,
    ).toBeNull();
  });

  it("deduplicates diagnostics without changing the response", () => {
    const result = performance();
    result.summary!.reasons = [incomplete];
    result.dataQuality.warnings = [incomplete];
    const original = structuredClone(result);
    expect(accountPerformanceReasons(result)).toEqual([incomplete]);
    accountLifetimeValues(valuation, result);
    expect(result).toEqual(original);
  });

  it("shows quality warnings, including an explicit reason when amount status is unavailable", () => {
    const result = performance();
    result.summary!.amountStatus = "unavailable";
    result.summary!.reasons = [incomplete];
    render(<AccountPerformanceWarnings performance={result} label="All-time Return" />);
    expect(screen.getByRole("note")).toHaveTextContent("All-time Return");
    expect(screen.getByText(incomplete)).toBeInTheDocument();
  });

  it("does not mount private diagnostics in hidden details or accessibility attributes", () => {
    const result = performance();
    result.dataQuality = {
      status: "partial",
      warnings: ["Synthetic private account example-account: residual 123.45."],
    };
    const { container, rerender } = render(
      <AccountPerformanceWarnings performance={result} label="3M" />,
    );
    expect(container.innerHTML).toContain("123.45");
    privacy.hidden = true;
    rerender(<AccountPerformanceWarnings performance={result} label="3M" />);
    expect(screen.getByRole("note")).toHaveTextContent("Some performance calculations");
    expect(container.innerHTML).not.toContain("123.45");
    expect(container.innerHTML).not.toContain("example-account");
    expect(container.querySelector("details")).toBeNull();
  });

  it("shows a generic error without exposing stale diagnostics", () => {
    const result = performance();
    result.dataQuality.warnings = [incomplete];
    render(<AccountPerformanceWarnings performance={result} label="All-time Return" failed />);
    expect(screen.getByText(/Error calculating performance data/)).toBeInTheDocument();
    expect(screen.queryByText(incomplete)).not.toBeInTheDocument();
  });

  it("does not add a warning or spacer for healthy data", () => {
    const { container } = render(
      <AccountPerformanceWarnings performance={performance()} label="3M" className="pb-3" />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
