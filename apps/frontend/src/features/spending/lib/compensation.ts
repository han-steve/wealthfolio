import type { CompensationDocument } from "../types/compensation";

// Only a documented gross amount is a headline, never a sum of overlapping wage bases.
export function documentedGross(document: CompensationDocument): number | null {
  const values = new Set(
    document.components
      .map((row) => row.metadata?.gross_pay_ytd)
      .filter((value): value is string => typeof value === "string"),
  );
  if (values.size !== 1) return null;
  const text = [...values][0];
  if (!/^\d+(\.\d+)?$/.test(text)) return null;
  const value = Number(text);
  return Number.isFinite(value) ? value : null;
}
