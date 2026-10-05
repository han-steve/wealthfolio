import { invoke } from "#platform";
import type { CompensationEvidence } from "../types/compensation";

export function getCompensationEvidence(request: { startDate: string; endDate: string }) {
  return invoke<CompensationEvidence>("get_compensation_evidence", { request });
}
