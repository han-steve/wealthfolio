import { useQuery } from "@tanstack/react-query";
import { getCompensationEvidence } from "../adapters/compensation";
import type { CompensationEvidence } from "../types/compensation";

export function useCompensationEvidence(request: { startDate: string; endDate: string }) {
  return useQuery<CompensationEvidence, Error>({
    queryKey: ["compensation-evidence", request],
    queryFn: () => getCompensationEvidence(request),
    refetchInterval: 5 * 60 * 1000,
    refetchOnWindowFocus: true,
  });
}
