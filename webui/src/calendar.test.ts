import { describe, expect, it } from "vitest";

import { buildRunCalendar, dayKey, shadeLevel, weekdayIndex, type RunDay } from "./calendar";

const END = new Date(2026, 9, 14); // Wednesday 2026-10-14, local time.

function gridFor(days: RunDay[] = [], weeks = 53) {
  return buildRunCalendar(days, { end: END, weeks, locale: "en-US" });
}

function cellFor(grid: ReturnType<typeof gridFor>, date: string) {
  return grid.weeks.flat().find((cell) => cell?.date === date);
}

describe("buildRunCalendar", () => {
  it("draws whole weeks, Monday first", () => {
    const grid = gridFor();
    expect(grid.weeks).toHaveLength(53);
    expect(grid.weeks.every((week) => week.length === 7)).toBe(true);
    expect(grid.weeks[0][0]?.date).toBe("2025-10-13"); // the Monday 52 weeks back
    expect(grid.weeks[0][0]?.date).toBe(dayKey(grid.start));
  });

  it("ends on today and leaves the rest of the week empty", () => {
    const last = gridFor().weeks.at(-1)!;
    expect(last[2]?.date).toBe("2026-10-14");
    // Thursday through Sunday have not happened yet.
    expect(last.slice(3)).toEqual([null, null, null, null]);
  });

  it("never marks the future as a zero day", () => {
    const grid = gridFor();
    const filled = grid.weeks.flat().filter((cell) => cell !== null);
    expect(filled.at(-1)?.date).toBe("2026-10-14");
  });

  it("maps a day's counts onto its own square", () => {
    const grid = gridFor([{ date: "2026-01-06", total: 4, failed: 1 }]);
    const cell = cellFor(grid, "2026-01-06");
    expect(cell).toEqual({ date: "2026-01-06", total: 4, failed: 1, level: 4 });
    expect(grid.max).toBe(4);
    // A day nothing ran on stays empty rather than borrowing a neighbour's count.
    expect(cellFor(grid, "2026-01-07")).toEqual({
      date: "2026-01-07",
      total: 0,
      failed: 0,
      level: 0,
    });
  });

  it("scales the shades to the busiest day in the window", () => {
    const grid = gridFor([
      { date: "2026-02-02", total: 8, failed: 0 },
      { date: "2026-02-03", total: 2, failed: 0 },
      { date: "2026-02-04", total: 5, failed: 0 },
    ]);
    expect(cellFor(grid, "2026-02-02")?.level).toBe(4);
    expect(cellFor(grid, "2026-02-03")?.level).toBe(1);
    expect(cellFor(grid, "2026-02-04")?.level).toBe(3);
  });

  it("labels every month once, including the partial first column", () => {
    const months = gridFor().months;
    expect(months[0]).toEqual({ label: "Oct", column: 0 });
    // 53 weeks spans 12 or 13 month starts; two labels on one month would mean
    // the boundary rule double-fires.
    expect(months.length).toBeGreaterThanOrEqual(12);
    expect(months.length).toBeLessThanOrEqual(13);
    const columns = months.map((month) => month.column);
    expect(new Set(columns).size).toBe(columns.length);
  });
});

describe("shadeLevel", () => {
  it("gives a single check-in a mid shade rather than the darkest", () => {
    expect(shadeLevel(1, 1)).toBe(2);
  });

  it("maps the range onto four steps when there is room", () => {
    expect(shadeLevel(0, 8)).toBe(0);
    expect(shadeLevel(1, 8)).toBe(1);
    expect(shadeLevel(4, 8)).toBe(2);
    expect(shadeLevel(6, 8)).toBe(3);
    expect(shadeLevel(8, 8)).toBe(4);
    // Counts above the recorded max (a day arriving after the max was taken)
    // never overflow the palette.
    expect(shadeLevel(12, 8)).toBe(4);
  });
});

describe("dayKey", () => {
  it("pads and stays on the local day", () => {
    expect(dayKey(new Date(2026, 0, 5, 23, 30))).toBe("2026-01-05");
    expect(weekdayIndex(new Date(2026, 9, 12))).toBe(0); // Monday
    expect(weekdayIndex(new Date(2026, 9, 18))).toBe(6); // Sunday
  });
});
