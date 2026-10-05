import { useTranslation } from "react-i18next";
import { PrivacyAmount } from "@wealthfolio/ui";
import type { CompensationEvidence } from "../../../types/compensation";
import { documentedGross } from "../../../lib/compensation";

interface Props {
  evidence?: CompensationEvidence;
  isLoading?: boolean;
  isError?: boolean;
}

export function CompensationOverview({ evidence, isLoading, isError }: Props) {
  const { t } = useTranslation();
  if (!evidence && !isLoading && !isError) return null;
  return (
    <section
      aria-label={t("spending:compensation.title")}
      className="border-border mt-6 border-t pt-5"
    >
      <h3 className="text-base font-semibold">{t("spending:compensation.title")}</h3>
      <p className="text-muted-foreground mt-1 text-xs">{t("spending:compensation.separate")}</p>
      {isLoading ? (
        <p className="mt-3 text-sm">{t("common:loading")}</p>
      ) : isError ? (
        <p className="mt-3 text-sm" role="alert">
          {t("spending:compensation.error")}
        </p>
      ) : !evidence?.documents.length ? (
        <p className="text-muted-foreground mt-3 text-sm">
          {t("spending:compensation.unavailable")}
        </p>
      ) : (
        evidence.documents.map((document) => {
          const gross = documentedGross(document);
          const groups = [...new Set(document.components.map((row) => row.componentGroup))];
          return (
            <div
              key={`${document.employer}-${document.taxYear}-${document.statementDate}-${document.currency}-${document.basis}`}
              className="border-border mt-4 border-t py-4 first:border-t-0"
            >
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h4 className="font-semibold">
                    {document.employer} · {document.taxYear} · {document.currency}
                  </h4>
                  <p className="text-muted-foreground text-xs">
                    {t(`spending:compensation.basis.${document.basis}`)} ·{" "}
                    {t("spending:compensation.asOf", { date: document.statementDate })}
                  </p>
                  {document.coverage.startDate && document.coverage.endDate && (
                    <p className="text-muted-foreground text-xs">
                      {document.coverage.startDate} → {document.coverage.endDate}
                    </p>
                  )}
                  {!document.coverage.matchesSelectedPeriod && (
                    <p className="text-muted-foreground mt-1 text-xs">
                      {t("spending:compensation.coverageMismatch")}
                    </p>
                  )}
                </div>
                {gross !== null && (
                  <div>
                    <div className="text-muted-foreground text-xs">
                      {t("spending:compensation.gross")}
                    </div>
                    <div className="text-xl font-semibold tabular-nums">
                      <PrivacyAmount value={gross} currency={document.currency} />
                    </div>
                  </div>
                )}
              </div>
              {groups.map((group) => (
                <details
                  key={group}
                  open={group === "gross_income" || group === "net_pay"}
                  className="mt-3"
                >
                  <summary className="cursor-pointer text-sm font-medium">
                    {t(`spending:compensation.groups.${group}`, {
                      defaultValue: group.replaceAll("_", " "),
                    })}
                  </summary>
                  {group === "tax" && (
                    <p className="text-muted-foreground mt-2 pl-4 text-xs">
                      {t("spending:compensation.taxNote")}
                    </p>
                  )}
                  <dl className="mt-2 space-y-1 pl-4">
                    {document.components
                      .filter((row) => row.componentGroup === group)
                      .map((row) => (
                        <div key={row.id} className="flex justify-between gap-4 text-sm">
                          <dt className="min-w-0 break-words">{row.componentName}</dt>
                          <dd className="shrink-0 tabular-nums">
                            <PrivacyAmount
                              value={Number(row.amountSigned)}
                              currency={document.currency}
                            />
                          </dd>
                        </div>
                      ))}
                  </dl>
                </details>
              ))}
            </div>
          );
        })
      )}
    </section>
  );
}
