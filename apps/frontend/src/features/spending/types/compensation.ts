export interface CompensationComponent {
  id: string;
  componentGroup: string;
  componentName: string;
  amountSigned: string;
  sourceBasis: string | null;
  metadata: Record<string, unknown> | null;
}

export interface CompensationDocument {
  statementDate: string;
  taxYear: number | null;
  employer: string;
  currency: string;
  basis: "annual" | "ytd" | "event" | "unknown";
  basisLabel: string;
  coverage: { startDate: string | null; endDate: string | null; matchesSelectedPeriod: boolean };
  components: CompensationComponent[];
}

export interface CompensationEvidence {
  status: "available" | "unavailable";
  unavailableReason: string | null;
  startDate: string;
  endDate: string;
  taxYears: number[];
  selectionPolicy: string;
  documents: CompensationDocument[];
}
