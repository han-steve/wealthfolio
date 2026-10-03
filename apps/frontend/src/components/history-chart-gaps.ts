import { addDays, differenceInCalendarDays, format, parseISO } from "date-fns";
import type { HistoryChartData } from "./history-chart";

export type HistoryChartPoint = Omit<HistoryChartData, "totalValue" | "netContribution"> & {
  totalValue: number | null;
  netContribution: number | null;
};

export function hasDailyHistoryGaps(data: { date: string }[]): boolean {
  return data.some(
    (point, index) =>
      index > 0 &&
      differenceInCalendarDays(parseISO(point.date), parseISO(data[index - 1].date)) > 1,
  );
}

export function withDailyHistoryGaps(data: HistoryChartData[]): HistoryChartPoint[] {
  const points: HistoryChartPoint[] = [];
  for (const point of data) {
    const previous = points.at(-1);
    if (previous) {
      // Keep missing days on the time axis without inventing portfolio values.
      for (
        let day = addDays(parseISO(previous.date), 1);
        day < parseISO(point.date);
        day = addDays(day, 1)
      ) {
        points.push({
          date: format(day, "yyyy-MM-dd"),
          currency: point.currency,
          totalValue: null,
          netContribution: null,
        });
      }
    }
    points.push(point);
  }
  return points;
}
