import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import { useBudget } from "@/features/spending/hooks/use-budget";
import { BudgetOverviewCard } from "./budget-overview-card";

vi.mock("@wealthfolio/ui", () => ({
  Button: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Icons: { Plus: () => <span />, AlertCircle: () => <span />, ChevronRight: () => <span /> },
  useAmountFormatting: () => ({
    formatRoundedAmount: (amount: number) => `$${amount}`,
  }),
}));
vi.mock("@/hooks/use-balance-privacy", () => ({
  useBalancePrivacy: () => ({ isBalanceHidden: false }),
}));
vi.mock("@/features/spending/hooks/use-budget", () => ({ useBudget: vi.fn() }));

describe("BudgetOverviewCard", () => {
  it("shows the recurring default, not a current-month override", () => {
    vi.mocked(useBudget).mockImplementation(
      (period) =>
        ({
          isLoading: false,
          data: {
            computed: {
              currency: "USD",
              totals: { spendingPlanned: period === "default" ? 250 : 999, incomePlanned: 0 },
              groupRows: [],
            },
          },
        }) as unknown as ReturnType<typeof useBudget>,
    );

    render(
      <MemoryRouter>
        <BudgetOverviewCard />
      </MemoryRouter>,
    );

    expect(useBudget).toHaveBeenCalledWith("default");
    expect(screen.getByText("$250")).toBeInTheDocument();
    expect(screen.queryByText("$999")).not.toBeInTheDocument();
  });
});
