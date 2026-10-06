import { formatZonedDateKey } from "@/features/spending/lib/timezone";
import { parseLocalDate } from "@/lib/utils";
import { calculatePerformanceSummary } from "@/adapters";
import { HistoryChart } from "@/components/history-chart";
import { hasDailyHistoryGaps } from "@/components/history-chart-gaps";
import { useHapticFeedback } from "@/hooks";
import { useCurrentValuation } from "@/hooks/use-current-account-valuations";
import { useHoldings } from "@/hooks/use-holdings";
import { useValuationHistory } from "@/hooks/use-valuation-history";
import { HoldingType, isAlternativeAssetKind } from "@/lib/constants";
import { performancePeriodPnl, performanceSummaryReturn } from "@/lib/performance";
import { QueryKeys } from "@/lib/query-keys";
import { useSettingsContext } from "@/lib/settings-provider";
import { PortfolioUpdateTrigger } from "@/pages/dashboard/portfolio-update-trigger";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import type { TimePeriod as UITimePeriod } from "@wealthfolio/ui";
import { GainAmount, GainPercent, getInitialIntervalData, IntervalSelector } from "@wealthfolio/ui";
import { usePersistentState } from "@/hooks/use-persistent-state";
import { Skeleton } from "@wealthfolio/ui/components/ui/skeleton";
import { Button } from "@wealthfolio/ui/components/ui/button";
import { format } from "date-fns";
import { History, TriangleAlert } from "lucide-react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { AccountsSummary } from "./accounts-summary";
import Balance from "./balance";
import SavingGoals from "./goals";
import TopHoldings from "./top-holdings";

const DEFAULT_INTERVAL: UITimePeriod = "3M";
const INTERVAL_STORAGE_KEY = "dashboard-interval";

function EmptyPortfolioHistory({
  isAllTime,
  onViewHistory,
}: {
  isAllTime: boolean;
  onViewHistory: () => void;
}) {
  const { t } = useTranslation();
  // Read the same complete-date series, without widening the selected chart or its returns.
  const { valuationHistory, isLoading, error } = useValuationHistory(
    undefined,
    { type: "all" },
    { enabled: !isAllTime },
  );
  const lastCompleteDate =
    !isLoading && !error ? valuationHistory?.at(-1)?.valuationDate : undefined;

  return (
    <div className="flex flex-col items-center gap-3">
      <p>{t("dashboard:history.empty")}</p>
      {!isAllTime && lastCompleteDate && (
        <p className="text-xs">{t("dashboard:history.through", { date: lastCompleteDate })}</p>
      )}
      <div className="flex flex-wrap justify-center gap-2">
        {!isAllTime && lastCompleteDate && (
          <Button variant="outline" size="sm" onClick={onViewHistory}>
            <History className="mr-2 size-4" aria-hidden="true" />
            {t("dashboard:history.view_available")}
          </Button>
        )}
        <Button variant="ghost" size="sm" asChild>
          <Link to="/health">
            <TriangleAlert className="mr-2 size-4" aria-hidden="true" />
            {t("common:component.data_status")}
          </Link>
        </Button>
      </div>
    </div>
  );
}

function getDashboardChartMinDomainSpanRatio(period: UITimePeriod): number {
  switch (period) {
    case "1D":
    case "1W":
      return 0.035;
    case "1M":
    case "3M":
      return 0.08;
    case "6M":
    case "YTD":
    case "1Y":
      return 0.16;
    case "5Y":
    case "ALL":
      return 0.2;
    default:
      return 0.12;
  }
}

function getDashboardNetContributionMaxDomainSpanRatio(period: UITimePeriod): number | undefined {
  switch (period) {
    case "1D":
    case "1W":
      return undefined;
    case "1M":
    case "3M":
      return 1.4;
    case "6M":
    case "YTD":
    case "1Y":
      return 2.2;
    case "5Y":
    case "ALL":
      return 2.8;
    default:
      return 1.8;
  }
}

export function DashboardContent() {
  const { t } = useTranslation();
  const { settings } = useSettingsContext();
  const todayISO = formatZonedDateKey(new Date(), settings?.timezone);
  const [selectedInterval, setSelectedInterval] = usePersistentState<UITimePeriod>(
    INTERVAL_STORAGE_KEY,
    DEFAULT_INTERVAL,
  );
  const dateRange = useMemo(
    () => getInitialIntervalData(selectedInterval, parseLocalDate(todayISO)).range,
    [selectedInterval, todayISO],
  );
  const isAllTime = selectedInterval === "ALL";

  const { holdings: allHoldings, isLoading: isHoldingsLoading } = useHoldings({ type: "all" });
  const {
    currentValuation: portfolioCurrentValuation,
    isLoading: isCurrentValuationLoading,
    error: currentValuationError,
  } = useCurrentValuation({ type: "all" }, { includeAccounts: true });
  const { triggerHaptic } = useHapticFeedback();

  // Filter holdings for display (exclude alternative assets and cash for TopHoldings)
  const holdings = useMemo(() => {
    if (!allHoldings) return [];
    return allHoldings.filter((h) => {
      // Exclude cash holdings from display
      if (h.holdingType === HoldingType.CASH) return false;
      // Exclude alternative assets from display
      if (h.assetKind && isAlternativeAssetKind(h.assetKind)) return false;
      return true;
    });
  }, [allHoldings]);

  const totalValue = portfolioCurrentValuation?.summary.totalValueBase ?? 0;

  const valuationHistoryRange = isAllTime ? undefined : dateRange;
  const {
    valuationHistory,
    isLoading: isValuationHistoryLoading,
    error: valuationHistoryError,
  } = useValuationHistory(valuationHistoryRange);

  const baseCurrency = settings?.baseCurrency ?? "USD";

  const startDate =
    !isAllTime && dateRange?.from ? format(dateRange.from, "yyyy-MM-dd") : undefined;
  const endDate = !isAllTime && dateRange?.to ? format(dateRange.to, "yyyy-MM-dd") : undefined;
  const datesReady = isAllTime || (!!startDate && !!endDate);

  const { data: portfolioPerformance, isLoading: isPortfolioPerformanceLoading } = useQuery({
    queryKey: [QueryKeys.PERFORMANCE_SUMMARY, "dashboard", "all", startDate, endDate],
    queryFn: () =>
      calculatePerformanceSummary({
        itemType: "account",
        itemId: "portfolio:all",
        startDate,
        endDate,
        filter: { type: "all" },
        profile: "dashboard",
      }),
    enabled: datesReady,
    placeholderData: keepPreviousData,
    staleTime: 30 * 1000,
    retry: 1,
  });

  const gainLossAmount = performancePeriodPnl(portfolioPerformance);
  const simpleReturn = performanceSummaryReturn(portfolioPerformance);
  const isCurrentValuationUnavailable =
    !isCurrentValuationLoading && !portfolioCurrentValuation && Boolean(currentValuationError);
  const portfolioSourceDataAsOf =
    portfolioCurrentValuation?.summary.sourceDataAsOf ??
    (!isCurrentValuationUnavailable
      ? valuationHistory?.[valuationHistory.length - 1]?.calculatedAt
      : undefined);

  const chartData = useMemo(() => {
    return (
      valuationHistory?.map((item) => ({
        date: item.valuationDate,
        totalValue: item.totalValueBase,
        netContribution: item.netContributionBase,
        currency: item.baseCurrency ?? baseCurrency,
      })) ?? []
    );
  }, [valuationHistory, baseCurrency]);

  const lastChartDate = chartData.at(-1)?.date;
  const chartHasGaps = useMemo(() => hasDailyHistoryGaps(chartData), [chartData]);
  const chartEndsEarly =
    lastChartDate && lastChartDate < (endDate ?? format(new Date(), "yyyy-MM-dd"));

  const chartMinDomainSpanRatio = useMemo(
    () => getDashboardChartMinDomainSpanRatio(selectedInterval),
    [selectedInterval],
  );
  const chartNetContributionMaxDomainSpanRatio = useMemo(
    () => getDashboardNetContributionMaxDomainSpanRatio(selectedInterval),
    [selectedInterval],
  );

  const isNegative = totalValue < 0;

  return (
    <div className="flex min-h-full flex-col">
      <div className="px-4 pb-1 pt-2 md:px-6 lg:px-8">
        <PortfolioUpdateTrigger
          lastCalculatedAt={portfolioSourceDataAsOf}
          notices={portfolioCurrentValuation?.summary.warnings}
        >
          <div className="flex items-start gap-2">
            <div>
              <Balance
                isLoading={isCurrentValuationLoading}
                isUnavailable={isCurrentValuationUnavailable}
                targetValue={totalValue}
                currency={baseCurrency}
                displayCurrency={true}
              />
              <div className="text-md flex min-h-5 flex-wrap items-center gap-x-3 gap-y-1">
                {isPortfolioPerformanceLoading ? (
                  <div className="flex items-center gap-3">
                    <Skeleton className="h-4 w-24" />
                    <div className="border-secondary my-1 border-r pr-2" />
                    <Skeleton className="h-4 w-16" />
                  </div>
                ) : gainLossAmount == null && simpleReturn == null ? (
                  <span className="text-muted-foreground lg:text-md text-sm font-light">
                    {t("dashboard:summary.returns_unavailable", "Returns unavailable")}
                  </span>
                ) : (
                  <>
                    {gainLossAmount == null ? (
                      <span className="text-muted-foreground lg:text-md text-sm font-light">
                        N/A
                      </span>
                    ) : (
                      <GainAmount
                        className="lg:text-md text-sm font-light"
                        value={gainLossAmount}
                        currency={baseCurrency}
                        displayCurrency={false}
                      />
                    )}
                    <div className="border-secondary my-1 border-r pr-2" />
                    {simpleReturn == null ? (
                      <span className="text-muted-foreground lg:text-md text-sm font-light">
                        N/A
                      </span>
                    ) : (
                      <GainPercent
                        className="lg:text-md text-sm font-light"
                        value={simpleReturn}
                        animated={true}
                      />
                    )}
                  </>
                )}
                {selectedInterval && (
                  <span className="lg:text-md text-muted-foreground ml-1 text-sm font-light">
                    {t(`ui:interval.${selectedInterval}`)}
                  </span>
                )}
              </div>
            </div>
          </div>
        </PortfolioUpdateTrigger>
      </div>

      <div
        className="flex grow flex-col"
        style={{
          backgroundImage: isNegative
            ? `linear-gradient(to top, color-mix(in srgb, var(--destructive) 30%, transparent), color-mix(in srgb, var(--destructive) 15%, transparent) 50%, transparent 100%)`
            : `linear-gradient(to top, color-mix(in srgb, var(--success) 30%, transparent), color-mix(in srgb, var(--success) 15%, transparent) 50%, transparent 100%)`,
        }}
      >
        <div>
          <div className="h-70">
            {chartData.length > 0 ? (
              <HistoryChart
                data={chartData}
                isLoading={isValuationHistoryLoading}
                showDailyGaps
                scaleMode="fit-visible"
                minDomainSpanRatio={chartMinDomainSpanRatio}
                netContributionMaxDomainSpanRatio={chartNetContributionMaxDomainSpanRatio}
              />
            ) : (
              <div
                role="status"
                className="text-muted-foreground flex h-full items-center justify-center px-4 text-center text-sm"
              >
                {isValuationHistoryLoading ? (
                  t("dashboard:history.loading", "Loading portfolio history...")
                ) : valuationHistoryError ? (
                  t("dashboard:history.error", "Portfolio history could not be loaded.")
                ) : (
                  <EmptyPortfolioHistory
                    isAllTime={isAllTime}
                    onViewHistory={() => setSelectedInterval("ALL")}
                  />
                )}
              </div>
            )}
          </div>
          {!isValuationHistoryLoading &&
            chartData.length > 0 &&
            (chartEndsEarly || chartHasGaps || valuationHistoryError) && (
              <p role="status" className="text-muted-foreground px-4 py-2 text-center text-xs">
                {valuationHistoryError
                  ? t(
                      "dashboard:history.refresh_error",
                      "History refresh failed; showing previously loaded data. ",
                    )
                  : null}
                {chartEndsEarly
                  ? t("dashboard:history.through", "Complete history through {{date}}. ", {
                      date: lastChartDate,
                    })
                  : null}
                {chartHasGaps
                  ? t("dashboard:history.gaps", "Gaps indicate missing account valuations.")
                  : null}
              </p>
            )}
          <div className="flex w-full justify-center">
            <IntervalSelector
              className="pointer-events-auto relative z-20 w-full max-w-screen-sm sm:max-w-screen-md md:max-w-2xl lg:max-w-3xl"
              onIntervalSelect={setSelectedInterval}
              onHaptic={triggerHaptic}
              isLoading={isValuationHistoryLoading}
              value={selectedInterval}
              defaultValue={DEFAULT_INTERVAL}
            />
          </div>
        </div>

        <div className="grow px-4 pb-[var(--mobile-nav-total-offset)] pt-6 md:px-6 md:pb-6 lg:px-10 lg:pb-8">
          <div className="grid grid-cols-1 gap-8 lg:grid-cols-3 lg:gap-20">
            <div className="lg:col-span-2">
              <AccountsSummary
                dateRange={dateRange}
                isAllTime={isAllTime}
                currentAccountValuations={portfolioCurrentValuation?.accounts}
                isLoadingCurrentValuations={isCurrentValuationLoading}
              />
            </div>
            <div className="space-y-6 lg:col-span-1">
              <TopHoldings
                holdings={holdings}
                isLoading={isHoldingsLoading}
                baseCurrency={baseCurrency}
              />
              <SavingGoals />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default DashboardContent;
