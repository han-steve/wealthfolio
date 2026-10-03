import { performancePeriodPnl } from "@/lib/performance";
import type { AccountValuation, PerformanceResult } from "@/lib/types";

export function accountPerformanceReasons(performance: PerformanceResult | null | undefined) {
  return Array.from(
    new Set([
      ...(performance?.summary?.reasons ?? []),
      ...(performance?.dataQuality.warnings ?? []),
      ...(performance?.dataQuality.notApplicableReasons ?? []),
    ]),
  );
}

export function accountPerformanceAmount(performance: PerformanceResult | null | undefined) {
  if (
    performance?.dataQuality.status === "noData" ||
    performance?.dataQuality.status === "notApplicable"
  ) {
    return null;
  }
  // The server can mark the amount complete even when this explicit diagnostic
  // says its attribution does not reconcile. Other partial results remain usable.
  const incompleteAttribution = accountPerformanceReasons(performance).some(
    (reason) =>
      reason.startsWith("Performance attribution is incomplete") ||
      reason.startsWith("Attribution residual "),
  );
  return incompleteAttribution ? null : performancePeriodPnl(performance);
}

export function accountLifetimeValues(
  valuation: AccountValuation | null | undefined,
  allTimePerformance: PerformanceResult | null | undefined,
) {
  const unavailable = { netContribution: null, returnAmount: null };
  if (
    !valuation ||
    allTimePerformance?.scope.id !== valuation.accountId ||
    accountPerformanceAmount(allTimePerformance) === null ||
    !Number.isFinite(valuation.totalValue) ||
    !Number.isFinite(valuation.netContribution)
  ) {
    return unavailable;
  }
  return {
    netContribution: valuation.netContribution,
    returnAmount: valuation.totalValue - valuation.netContribution,
  };
}
