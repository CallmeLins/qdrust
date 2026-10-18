/**
 * Layout for the contribution-style check-in calendar.
 *
 * Kept out of the component for the same reason `pagination.ts` is: the grid is
 * arithmetic — which column a day lands in, how dark a square is, where a month
 * label goes — and arithmetic can be tested without a browser. This repo has no
 * component test harness, so anything that lives in a template is untested by
 * default.
 *
 * Days arrive from the server already bucketed in the reader's timezone, keyed
 * by `YYYY-MM-DD`. Nothing here converts timestamps: doing that twice is how a
 * check-in at 00:30 ends up in the previous square.
 */

/** One day's counts as `GET /api/v1/runs/calendar` reports them. */
export interface RunDay {
  date: string;
  total: number;
  failed: number;
}

export interface CalendarCell {
  /** `YYYY-MM-DD`. */
  date: string;
  total: number;
  failed: number;
  /** 0 (no runs) to 4 (the busiest day in the window). */
  level: number;
}

export interface CalendarGrid {
  /** Week columns, Monday first. `null` is a square after today. */
  weeks: (CalendarCell | null)[][];
  /** Month labels and the column each starts at. */
  months: { label: string; column: number }[];
  /** First day drawn, for the request window. */
  start: Date;
  /** Last day drawn (today, local midnight). */
  end: Date;
  /** Highest daily count in the window; 0 when nothing ran. */
  max: number;
}

/** Local midnight of the given date, so day arithmetic cannot drift across a
 *  DST boundary or a stored time-of-day. */
function startOfDay(value: Date): Date {
  return new Date(value.getFullYear(), value.getMonth(), value.getDate());
}

/** `YYYY-MM-DD` in local time. Not `toISOString()`, which shifts to UTC and
 *  would name the previous day for anyone east of Greenwich after midnight. */
export function dayKey(value: Date): string {
  const month = String(value.getMonth() + 1).padStart(2, "0");
  const day = String(value.getDate()).padStart(2, "0");
  return `${value.getFullYear()}-${month}-${day}`;
}

/** Monday = 0 … Sunday = 6. `getDay()` starts on Sunday, the grid does not. */
export function weekdayIndex(value: Date): number {
  return (value.getDay() + 6) % 7;
}

function addDays(value: Date, days: number): Date {
  return new Date(value.getFullYear(), value.getMonth(), value.getDate() + days);
}

/**
 * Build the grid ending on the week that contains `end`.
 *
 * Columns are weeks, rows are weekdays, and the last column is padded with
 * `null` for the days still to come — drawing them as "no runs" would claim
 * the future is empty.
 */
export function buildRunCalendar(
  days: RunDay[],
  options: { end: Date; weeks: number; locale?: string },
): CalendarGrid {
  const end = startOfDay(options.end);
  const weeks = Math.max(1, Math.floor(options.weeks));
  const locale = options.locale ?? "zh-CN";

  const byDate = new Map(days.map((day) => [day.date, day]));
  const max = days.reduce((highest, day) => Math.max(highest, day.total), 0);

  // The last column is the current week, so the grid ends on the week's Monday
  // plus six — not on today.
  const thisMonday = addDays(end, -weekdayIndex(end));
  const start = addDays(thisMonday, -(weeks - 1) * 7);

  const columns: (CalendarCell | null)[][] = [];
  const months: { label: string; column: number }[] = [];
  const monthFormat = new Intl.DateTimeFormat(locale, { month: "short" });
  let labelledMonth = -1;

  for (let column = 0; column < weeks; column += 1) {
    const week: (CalendarCell | null)[] = [];
    for (let row = 0; row < 7; row += 1) {
      const date = addDays(start, column * 7 + row);
      if (date.getTime() > end.getTime()) {
        week.push(null);
        continue;
      }
      const key = dayKey(date);
      const stat = byDate.get(key);
      const total = stat?.total ?? 0;
      week.push({
        date: key,
        total,
        failed: stat?.failed ?? 0,
        level: shadeLevel(total, max),
      });
    }
    // One label per month, anchored on the first column that shows it. The
    // first column is labelled even though it is usually a partial month —
    // leaving it blank makes the axis look like it starts later than it does.
    const firstOfColumn = addDays(start, column * 7);
    if (column === 0 || firstOfColumn.getMonth() !== labelledMonth) {
      labelledMonth = firstOfColumn.getMonth();
      months.push({ label: monthFormat.format(firstOfColumn), column });
    }
    columns.push(week);
  }

  return { weeks: columns, months, start, end, max };
}

/**
 * Five shades: none, then four steps scaled to the busiest day in the window.
 *
 * Relative rather than fixed thresholds because "five check-ins" is a quiet day
 * for someone running thirty tasks and a busy day for someone running two. One
 * run is never the darkest square, however light the week is.
 */
export function shadeLevel(total: number, max: number): number {
  if (total <= 0) return 0;
  if (max <= 1) return 2;
  return Math.min(4, Math.max(1, Math.ceil((total / max) * 4)));
}
