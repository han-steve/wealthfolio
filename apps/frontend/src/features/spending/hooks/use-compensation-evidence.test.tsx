import { QueryClient, QueryClientProvider, focusManager } from "@tanstack/react-query";
import { act, cleanup, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryKeys } from "@/lib/query-keys";
import type { CompensationEvidence } from "../types/compensation";
import { useCashActivitySearch } from "./use-cash-activity-search";
import { useCompensationEvidence } from "./use-compensation-evidence";

const adapterMocks = vi.hoisted(() => ({
  getCompensationEvidence:
    vi.fn<(request: { startDate: string; endDate: string }) => Promise<CompensationEvidence>>(),
  searchCashActivities: vi.fn(),
}));

vi.mock("../adapters/compensation", () => ({
  getCompensationEvidence: adapterMocks.getCompensationEvidence,
}));
vi.mock("../adapters/cash-activities", () => ({
  searchCashActivities: adapterMocks.searchCashActivities,
}));

const request = { startDate: "2025-01-01", endDate: "2025-09-30" };

function evidenceFor(range = request, amount = "1000"): CompensationEvidence {
  return {
    status: "available",
    unavailableReason: null,
    ...range,
    taxYears: [2025],
    selectionPolicy: "As-of documents",
    documents: [
      {
        employer: "Example employer",
        taxYear: 2025,
        statementDate: range.endDate,
        currency: "USD",
        basis: "ytd",
        basisLabel: "YTD",
        coverage: { ...range, matchesSelectedPeriod: true },
        components: [
          {
            id: "salary",
            componentGroup: "gross_income",
            componentName: "Regular wages",
            amountSigned: amount,
            sourceBasis: "Paystub YTD",
            metadata: { gross_pay_ytd: amount },
          },
        ],
      },
    ],
  };
}

function createWrapper(queryClient: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  };
}

async function flushQueries() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1);
  });
}

describe("useCompensationEvidence", () => {
  let queryClient: QueryClient;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2025-10-01T12:00:00Z"));
    vi.resetAllMocks();
    focusManager.setFocused(true);
    queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, refetchOnWindowFocus: false, staleTime: 5 * 60 * 1000 },
      },
    });
    queryClient.setQueryDefaults([QueryKeys.ACTIVITIES], { gcTime: Infinity });
    queryClient.setQueryData([QueryKeys.ACTIVITIES], [{ id: "existing-ledger-activity" }]);
    adapterMocks.getCompensationEvidence.mockResolvedValue(evidenceFor());
    adapterMocks.searchCashActivities.mockResolvedValue({
      items: [],
      totalCount: 0,
      net: { byCurrency: [{ currency: "USD", amount: 125 }] },
    });
  });

  afterEach(() => {
    cleanup();
    queryClient.clear();
    focusManager.setFocused(undefined);
    vi.useRealTimers();
  });

  it("does not reread evidence after thirty foreground minutes or change cash queries", async () => {
    const ledgerState = queryClient.getQueryState([QueryKeys.ACTIVITIES]);
    const { result } = renderHook(
      () => ({ evidence: useCompensationEvidence(request), cash: useCashActivitySearch({}) }),
      { wrapper: createWrapper(queryClient) },
    );
    await flushQueries();
    expect(result.current.evidence.data).toEqual(evidenceFor());
    const cashNet = result.current.cash.net;
    const cashState = queryClient
      .getQueryCache()
      .getAll()
      .find((query) => query.queryKey.includes("search"))?.state;
    expect(cashNet).toEqual({ byCurrency: [{ currency: "USD", amount: 125 }] });

    adapterMocks.getCompensationEvidence.mockResolvedValue(evidenceFor(request, "1200"));
    await act(() => vi.advanceTimersByTimeAsync(30 * 60 * 1000));
    await flushQueries();

    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
    expect(adapterMocks.getCompensationEvidence).toHaveBeenLastCalledWith(request);
    expect(result.current.evidence.data).toEqual(evidenceFor());
    expect(result.current.cash.net).toBe(cashNet);
    expect(
      queryClient
        .getQueryCache()
        .getAll()
        .find((query) => query.queryKey.includes("search"))?.state,
    ).toBe(cashState);
    expect(adapterMocks.searchCashActivities).toHaveBeenCalledTimes(1);
    expect(queryClient.getQueryState([QueryKeys.ACTIVITIES])).toBe(ledgerState);
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("fetches a newly selected date range once and keeps separate cached evidence", async () => {
    const nextRequest = { startDate: "2025-04-01", endDate: "2025-06-30" };
    adapterMocks.getCompensationEvidence.mockImplementation((range) =>
      Promise.resolve(evidenceFor(range)),
    );
    const { result, rerender } = renderHook((range) => useCompensationEvidence(range), {
      initialProps: request,
      wrapper: createWrapper(queryClient),
    });
    await flushQueries();
    rerender(nextRequest);
    await flushQueries();

    expect(result.current.data).toEqual(evidenceFor(nextRequest));
    expect(queryClient.getQueryData(["compensation-evidence", request])).toEqual(evidenceFor());
    expect(queryClient.getQueryData(["compensation-evidence", nextRequest])).toEqual(
      evidenceFor(nextRequest),
    );
    rerender({ ...nextRequest });
    await flushQueries();
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(2);
    await act(() => vi.advanceTimersByTimeAsync(30 * 60 * 1000));
    await flushQueries();
    expect(adapterMocks.getCompensationEvidence.mock.calls.map(([range]) => range)).toEqual([
      request,
      nextRequest,
    ]);
  });

  it("does not reread evidence after thirty background minutes", async () => {
    renderHook(() => useCompensationEvidence(request), { wrapper: createWrapper(queryClient) });
    await flushQueries();
    act(() => focusManager.setFocused(false));
    await act(() => vi.advanceTimersByTimeAsync(30 * 60 * 1000));
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
  });

  it("refreshes stale evidence on focus despite the global focus-refetch default", async () => {
    const ledgerState = queryClient.getQueryState([QueryKeys.ACTIVITIES]);
    const { result } = renderHook(
      () => ({ evidence: useCompensationEvidence(request), cash: useCashActivitySearch({}) }),
      { wrapper: createWrapper(queryClient) },
    );
    await flushQueries();
    const cashNet = result.current.cash.net;
    const cashQuery = queryClient
      .getQueryCache()
      .getAll()
      .find((query) => query.queryKey.includes("search"));
    const cashState = cashQuery?.state;
    act(() => focusManager.setFocused(false));
    adapterMocks.getCompensationEvidence.mockResolvedValue(evidenceFor(request, "1200"));
    await act(() => vi.advanceTimersByTimeAsync(300_001));
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
    act(() => focusManager.setFocused(true));
    await flushQueries();

    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(2);
    expect(result.current.evidence.data).toEqual(evidenceFor(request, "1200"));
    expect(result.current.cash.net).toBe(cashNet);
    expect(cashQuery?.state).toBe(cashState);
    expect(adapterMocks.searchCashActivities).toHaveBeenCalledTimes(1);
    expect(queryClient.getQueryState([QueryKeys.ACTIVITIES])).toBe(ledgerState);
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("does not reread fresh evidence on focus or remount", async () => {
    const wrapper = createWrapper(queryClient);
    const { unmount } = renderHook(() => useCompensationEvidence(request), { wrapper });
    await flushQueries();
    await act(() => vi.advanceTimersByTimeAsync(60_000));
    act(() => focusManager.setFocused(false));
    act(() => focusManager.setFocused(true));
    await flushQueries();
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);

    unmount();
    const { result } = renderHook(() => useCompensationEvidence(request), { wrapper });
    await flushQueries();
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
    expect(result.current.data).toEqual(evidenceFor());
  });

  it("refreshes stale cached evidence on remount", async () => {
    const wrapper = createWrapper(queryClient);
    const { unmount } = renderHook(() => useCompensationEvidence(request), { wrapper });
    await flushQueries();
    await act(() => vi.advanceTimersByTimeAsync(300_001));
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
    unmount();
    expect(queryClient.getQueryData(["compensation-evidence", request])).toEqual(evidenceFor());

    adapterMocks.getCompensationEvidence.mockResolvedValue(evidenceFor(request, "1200"));
    const { result } = renderHook(() => useCompensationEvidence(request), { wrapper });
    await flushQueries();
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(2);
    expect(adapterMocks.getCompensationEvidence).toHaveBeenLastCalledWith(request);
    expect(result.current.data).toEqual(evidenceFor(request, "1200"));
  });

  it("allows manual refresh of fresh evidence without changing cash queries or mutating", async () => {
    const ledgerState = queryClient.getQueryState([QueryKeys.ACTIVITIES]);
    const { result } = renderHook(
      () => ({ evidence: useCompensationEvidence(request), cash: useCashActivitySearch({}) }),
      { wrapper: createWrapper(queryClient) },
    );
    await flushQueries();
    expect(result.current.evidence.data).toEqual(evidenceFor());
    const cashNet = result.current.cash.net;
    const cashQuery = queryClient
      .getQueryCache()
      .getAll()
      .find((query) => query.queryKey.includes("search"));
    const cashState = cashQuery?.state;
    adapterMocks.getCompensationEvidence.mockResolvedValue(evidenceFor(request, "1200"));

    await act(async () => {
      await result.current.evidence.refetch();
    });
    await flushQueries();

    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(2);
    expect(adapterMocks.getCompensationEvidence).toHaveBeenLastCalledWith(request);
    expect(result.current.evidence.data).toEqual(evidenceFor(request, "1200"));
    expect(result.current.cash.net).toBe(cashNet);
    expect(cashQuery?.state).toBe(cashState);
    expect(adapterMocks.searchCashActivities).toHaveBeenCalledTimes(1);
    expect(queryClient.getQueryState([QueryKeys.ACTIVITIES])).toBe(ledgerState);
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });
});
