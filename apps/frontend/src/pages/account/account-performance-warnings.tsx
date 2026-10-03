import type { PerformanceResult } from "@/lib/types";
import { cn } from "@/lib/utils";
import { useBalancePrivacy } from "@wealthfolio/ui";
import { useTranslation } from "react-i18next";
import { accountPerformanceReasons } from "./account-performance-quality";

interface AccountPerformanceWarningsProps {
  performance?: PerformanceResult | null;
  label: string;
  failed?: boolean;
  className?: string;
}

export function AccountPerformanceWarnings({
  performance,
  label,
  failed = false,
  className,
}: AccountPerformanceWarningsProps) {
  const { t } = useTranslation();
  const { isBalanceHidden } = useBalancePrivacy();
  const limited =
    performance?.dataQuality.status === "partial" ||
    performance?.dataQuality.status === "noData" ||
    performance?.summary?.quality === "partial" ||
    performance?.summary?.amountStatus === "unavailable" ||
    Boolean(performance?.dataQuality.warnings?.length);
  if (!limited && !failed) return null;

  // Diagnostics can contain account IDs and monetary residuals. Do not mount
  // them in hidden details or accessibility text while balance privacy is on.
  const reasons = isBalanceHidden || failed ? [] : accountPerformanceReasons(performance);
  return (
    <div
      role="note"
      className={cn("text-warning min-w-0 space-y-1 break-words text-xs", className)}
    >
      <p>
        <span className="font-medium">{label}: </span>
        {t(failed ? "performance:error_calculating" : "performance:data_quality.caution")}
      </p>
      {reasons.length > 0 && (
        <details>
          <summary className="cursor-pointer">{t("performance:data_quality.limited")}</summary>
          <ul className="list-disc space-y-1 py-1 pl-4">
            {reasons.map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
