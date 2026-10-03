import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import type { TaxonomyCategory } from "@/lib/types";
import type { BudgetSnapshot } from "../../../types/budget";
import { FormattingProvider } from "@wealthfolio/ui";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { WhereIAmStage } from "./where-i-am-stage";
import type { MonthlyReport } from "../../../types/report";
import { comparisonRange } from "../../../lib/reports-period";
import { spendingRangeToReportsRange } from "../../../lib/date-range-params";

const table = vi.hoisted(() => vi.fn((_props: Record<string, unknown>) => null));
vi.mock("../category-hierarchy-table", () => ({ CategoryHierarchyTable: table }));

vi.mock("@/hooks/use-balance-privacy", () => ({
  useBalancePrivacy: () => ({ isBalanceHidden: false }),
}));

vi.mock("../../../hooks/use-spending-settings", () => ({
  useSpendingSettings: () => ({ excludedCategoryIds: [] }),
}));

function report(outflow: number): MonthlyReport {
  const summary = { income: 0, outflow, saved: 0, net: -outflow, count: 1 };
  return {
    baseCurrency: "USD",
    current: summary,
    prior: summary,
    spendingBreakdown: [],
    incomeBreakdown: [],
    savingsBreakdown: [],
    byDay: [],
    byDayByCategory: [],
  };
}

const timezone = "America/Toronto";
const range = spendingRangeToReportsRange(
  { from: new Date(2025, 2, 8), to: new Date(2025, 2, 10) },
  timezone,
);

function setup(custom: boolean, overrides: Partial<ComponentProps<typeof WhereIAmStage>> = {}) {
  render(
    <MemoryRouter>
      <FormattingProvider locale="en-US" timezone={timezone}>
        <WhereIAmStage
          range={range}
          priorRange={custom ? comparisonRange(range, "prior", timezone)! : undefined}
          currentReport={report(300)}
          priorReport={report(150)}
          months={[]}
          taxonomyCategories={[]}
          incomeCategories={[]}
          savingsCategories={[]}
          budget={undefined}
          currency="USD"
          isLoading={false}
          {...overrides}
        />
      </FormattingProvider>
    </MemoryRouter>,
  );
}

describe("Where I am comparison labels", () => {
  it.each([0, 50, -50])(
    "itemizes signed income corrections with net income %s, without positive-only shares",
    (net) => {
      const categories = [
        { id: "parent", name: "Employment", parentId: null },
        ...Array.from({ length: 5 }, (_, i) => ({
          id: `source-${i}`,
          name: `Source ${i}`,
          parentId: "parent",
        })),
        { id: "correction", name: "Income tax correction", parentId: "parent" },
      ] as TaxonomyCategory[];
      const amounts = [100, 80, 60, 40, 20];
      setup(false, {
        incomeCategories: categories,
        months: [
          {
            iso: "2025-03-01",
            label: "Mar",
            isLoading: false,
            report: {
              ...report(30),
              current: { income: net, outflow: 30, saved: 0, net: net - 30, count: 6 },
            },
          },
        ],
        currentReport: {
          ...report(30),
          current: { income: net, outflow: 30, saved: 0, net: net - 30, count: 6 },
          incomeBreakdown: [
            ...amounts.map((amount, i) => ({
              categoryId: `source-${i}`,
              taxonomyId: "income_sources",
              amount,
              count: 1,
            })),
            { categoryId: "correction", taxonomyId: "income_sources", amount: net - 300, count: 1 },
          ],
        },
      });
      const cashflow = within(screen.getByText("Net income").closest("section")!);
      expect(cashflow.getByText("Income tax correction")).toBeInTheDocument();
      expect(cashflow.getByText(`-$${300 - net}.00`)).toBeInTheDocument();
      expect(cashflow.getByText(net < 0 ? `-$${-net}.00` : `$${net}.00`)).toBeInTheDocument();
      for (let i = 0; i < 5; i++) expect(cashflow.getByText(`Source ${i}`)).toBeInTheDocument();
      expect(cashflow.queryByText("Employment")).not.toBeInTheDocument();
      expect(cashflow.queryByText("Money in")).not.toBeInTheDocument();
      expect(cashflow.queryByText(/%/)).not.toBeInTheDocument();
      expect(
        screen.queryByText("No income in selected accounts for this period."),
      ).not.toBeInTheDocument();
      const headline = screen.getByText("NET CASHFLOW").closest("div[class*='rounded']")!;
      for (const bar of headline.querySelectorAll<HTMLElement>("[style*='width']")) {
        expect(parseFloat(bar.style.width)).toBeGreaterThanOrEqual(0);
        expect(parseFloat(bar.style.width)).toBeLessThanOrEqual(100);
      }
    },
  );

  it("counts visible prior-only and budget-only categories as well as current spending", () => {
    setup(false, {
      currentReport: {
        ...report(150),
        spendingBreakdown: [{ categoryId: "a", taxonomyId: "spending", amount: 150, count: 1 }],
      },
      priorReport: {
        ...report(80),
        spendingBreakdown: [{ categoryId: "b", taxonomyId: "spending", amount: 80, count: 1 }],
      },
      taxonomyCategories: [{ id: "c", name: "Budget only", parentId: null }] as TaxonomyCategory[],
      budget: {
        computed: {
          totals: { spendingPlanned: 200 },
          groupRows: [
            {
              group: { id: "group", key: "needs" },
              categories: [{ categoryId: "c", target: 200 }],
            },
          ],
        },
      } as BudgetSnapshot,
    });
    expect(screen.getAllByText("3 of 3 categories shown").length).toBeGreaterThan(0);
  });
  it("labels a custom span as a period and shows the actual prior dates across DST", () => {
    setup(true);
    expect(screen.getByText("SPENT THIS PERIOD")).toBeInTheDocument();
    expect(screen.getByText(/vs Mar 5, 2025 – Mar 7, 2025/)).toBeInTheDocument();
    expect(screen.queryByText("SPENT THIS MONTH")).not.toBeInTheDocument();
    expect(screen.queryByText(/vs Feb/)).not.toBeInTheDocument();
  });
  it("retains month labels for preset month selections", () => {
    setup(false, {
      range: spendingRangeToReportsRange(
        { from: new Date(2025, 2, 1), to: new Date(2025, 2, 10) },
        timezone,
      ),
    });
    expect(screen.getByText("SPENT THIS MONTH")).toBeInTheDocument();
    expect(screen.getByText(/vs Feb/)).toBeInTheDocument();
  });

  it("does not label a rolling year as thirteen months", () => {
    setup(false, {
      range: spendingRangeToReportsRange(
        { from: new Date(2025, 9, 3), to: new Date(2026, 9, 3) },
        timezone,
      ),
    });
    expect(screen.getByText("SPENT THIS PERIOD")).toBeInTheDocument();
    expect(screen.queryByText(/SPENT.*13 MO/)).not.toBeInTheDocument();
  });

  it("filters current, prior and budget categories together", async () => {
    const rows = [
      { taxonomyId: "spending", categoryId: "a", amount: 150, count: 1 },
      { taxonomyId: "spending", categoryId: "b", amount: 30, count: 1 },
    ];
    const budgetRows = rows.map((row) => ({ categoryId: row.categoryId, target: 100 }));
    setup(false, {
      currentReport: { ...report(180), spendingBreakdown: rows },
      priorReport: {
        ...report(80),
        spendingBreakdown: rows.map((row) => ({ ...row, amount: 40 })),
      },
      budget: {
        computed: {
          totals: { spendingPlanned: 200 },
          groupRows: [{ group: { id: "group", key: "needs" }, categories: budgetRows }],
        },
      } as BudgetSnapshot,
    });
    await userEvent.click(screen.getByRole("button", { name: /Over budget/ }));
    const props = table.mock.calls.at(-1)?.[0] as unknown as {
      breakdown: typeof rows;
      priorBreakdown: typeof rows;
      budgetRows: typeof budgetRows;
      groupRows: { categories: typeof budgetRows }[];
    };
    expect(props.breakdown.map((row) => row.categoryId)).toEqual(["a"]);
    expect(props.priorBreakdown.map((row) => row.categoryId)).toEqual(["a"]);
    expect(props.budgetRows.map((row) => row.categoryId)).toEqual(["a"]);
    expect(props.groupRows[0].categories.map((row) => row.categoryId)).toEqual(["a"]);
  });
});
