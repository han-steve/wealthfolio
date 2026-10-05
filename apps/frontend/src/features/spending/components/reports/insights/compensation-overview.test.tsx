import { render, screen, within } from "@testing-library/react";
import { FormattingProvider } from "@wealthfolio/ui";
import { beforeEach, describe, expect, it } from "vitest";
import { CompensationOverview } from "./compensation-overview";
import { documentedGross } from "../../../lib/compensation";
import type { CompensationDocument, CompensationEvidence } from "../../../types/compensation";

const document: CompensationDocument = {
  employer: "Example employer",
  taxYear: 2025,
  statementDate: "2025-09-30",
  currency: "USD",
  basis: "ytd",
  basisLabel: "YTD",
  coverage: { startDate: "2025-01-01", endDate: "2025-09-30", matchesSelectedPeriod: false },
  components: [
    {
      id: "salary",
      componentGroup: "gross_income",
      componentName: "Regular wages",
      amountSigned: "7000",
      sourceBasis: "Paystub YTD",
      metadata: { gross_pay_ytd: "10000" },
    },
    {
      id: "rsu",
      componentGroup: "gross_income",
      componentName: "RSU income",
      amountSigned: "3000",
      sourceBasis: "Paystub YTD",
      metadata: { gross_pay_ytd: "10000" },
    },
    {
      id: "net",
      componentGroup: "net_pay",
      componentName: "Net direct deposits",
      amountSigned: "4000",
      sourceBasis: "Paystub YTD",
      metadata: {},
    },
    {
      id: "tax",
      componentGroup: "tax",
      componentName: "Income tax",
      amountSigned: "-2000",
      sourceBasis: "Paystub YTD",
      metadata: {},
    },
    {
      id: "base",
      componentGroup: "taxable_wages",
      componentName: "Federal wage base",
      amountSigned: "9000",
      sourceBasis: "Paystub YTD",
      metadata: {},
    },
  ],
};
const evidence: CompensationEvidence = {
  status: "available",
  unavailableReason: null,
  startDate: "2025-07-01",
  endDate: "2025-09-30",
  taxYears: [2025],
  selectionPolicy: "As-of documents",
  documents: [document],
};
function setup(props: Parameters<typeof CompensationOverview>[0]) {
  render(
    <FormattingProvider locale="en-US" timezone="America/Toronto">
      <CompensationOverview {...props} />
    </FormattingProvider>,
  );
}

describe("Native spending compensation", () => {
  beforeEach(() => localStorage.clear());
  it("hides gross, RSU and net-pay amounts with balance privacy", () => {
    localStorage.setItem("privacy-settings", "true");
    setup({ evidence });
    expect(screen.queryByText("$10,000.00")).not.toBeInTheDocument();
    expect(screen.queryByText("$3,000.00")).not.toBeInTheDocument();
    expect(screen.queryByText("$4,000.00")).not.toBeInTheDocument();
    expect(screen.getAllByText("••••")).toHaveLength(6);
  });
  it("itemizes documented gross and RSUs without adding net pay or overlapping wage bases", () => {
    setup({ evidence });
    const section = screen.getByRole("region", { name: "Gross compensation" });
    expect(within(section).getByText("$10,000.00")).toBeInTheDocument();
    expect(within(section).getByText("RSU income")).toBeInTheDocument();
    expect(within(section).getByText("Net direct deposits")).toBeInTheDocument();
    expect(within(section).getByText(/not gross income for that range/)).toBeInTheDocument();
    expect(within(section).getByText(/overlapping, not additive/)).toBeInTheDocument();
    expect(within(section).queryByText("$23,000.00")).not.toBeInTheDocument();
  });
  it("does not fabricate gross wages from annual taxable wages or conflicting evidence", () => {
    expect(documentedGross({ ...document, components: document.components.map((row) => ({ ...row, metadata: null })) })).toBeNull();
    expect(
      documentedGross({
        ...document,
        components: document.components.map((row) => ({ ...row, metadata: {} })),
      }),
    ).toBeNull();
    expect(
      documentedGross({
        ...document,
        components: [
          ...document.components,
          { ...document.components[0], metadata: { gross_pay_ytd: "9999" } },
        ],
      }),
    ).toBeNull();
  });
  it("shows missing evidence rather than zero salary", () => {
    setup({ evidence: { ...evidence, status: "unavailable", documents: [] } });
    expect(screen.getByText(/No compensation document is available/)).toBeInTheDocument();
    expect(screen.queryByText("$0.00")).not.toBeInTheDocument();
  });
  it("shows failure separately from cash reporting", () => {
    setup({ isError: true });
    expect(screen.getByRole("alert")).toHaveTextContent("Cash income is unchanged");
  });
});
