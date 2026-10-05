import { QueryClient, QueryClientProvider, focusManager } from "@tanstack/react-query";
import { act, cleanup, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
    await vi.advanceTimersByTimeAsync(0);
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

  it("polls the local evidence adapter every five minutes without changing cash queries", async () => {
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
    await act(() => vi.advanceTimersByTimeAsync(299_999));
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
    await act(() => vi.advanceTimersByTimeAsync(1));
    await flushQueries();

    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(2);
    expect(adapterMocks.getCompensationEvidence).toHaveBeenLastCalledWith(request);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(result.current.evidence.data).toEqual(evidenceFor(request, "1200"));
    expect(result.current.cash.net).toBe(cashNet);
    expect(
      queryClient
        .getQueryCache()
        .getAll()
        .find((query) => query.queryKey.includes("search"))?.state,
    ).toBe(cashState);
    expect(adapterMocks.searchCashActivities).toHaveBeenCalledTimes(1);
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("keeps the date-scoped query key and polls only the newly selected range", async () => {
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
    await act(() => vi.advanceTimersByTimeAsync(300_000));
    await flushQueries();
    expect(adapterMocks.getCompensationEvidence.mock.calls.map(([range]) => range)).toEqual([
      request,
      nextRequest,
      nextRequest,
    ]);
  });

  it("does not poll while the window is in the background", async () => {
    renderHook(() => useCompensationEvidence(request), { wrapper: createWrapper(queryClient) });
    await flushQueries();
    act(() => focusManager.setFocused(false));
    await act(() => vi.advanceTimersByTimeAsync(600_000));
    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(1);
  });

  it("refreshes stale evidence on focus despite the global focus-refetch default", async () => {
    const { result } = renderHook(() => useCompensationEvidence(request), {
      wrapper: createWrapper(queryClient),
    });
    await flushQueries();
    act(() => focusManager.setFocused(false));
    adapterMocks.getCompensationEvidence.mockResolvedValue(evidenceFor(request, "1200"));
    await act(() => vi.advanceTimersByTimeAsync(300_001));
    act(() => focusManager.setFocused(true));
    await flushQueries();

    expect(adapterMocks.getCompensationEvidence).toHaveBeenCalledTimes(2);
    expect(result.current.data).toEqual(evidenceFor(request, "1200"));
  });
});
