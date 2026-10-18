import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Where a scroll container is allowed to live.
 *
 * `.modal` is shared by two things that want opposite treatment. A dialog the
 * backdrop centres has to be capped to the viewport and scroll inside itself,
 * or a tall one runs off the screen. A `.inline-modal` is an ordinary card in
 * the page it sits in — the notification page's plugin, channel, action and
 * push forms — and has to scroll with that page instead: the page is already
 * scrolling, and the action form additionally carries its own `.check-list`
 * scroll box, so a cap on the card drew a second scrollbar next to the list's.
 * Handing the card the same cap is also what kept issue #12 alive: a scroll
 * container counts an absolutely positioned dropdown list as part of its
 * scrollable overflow, which pushed that list below the fold, and it took
 * teleporting the list out of the container to stop it.
 *
 * The split is a CSS-only contract, so the assertions below read style.css.
 * They pin the shape of the rules, not a rendered result: the visible outcome
 * still needs a browser, which this project does not have in CI.
 */
const css = readFileSync(new URL("./style.css", import.meta.url), "utf8");

interface Rule {
  selector: string;
  body: string;
}

/**
 * Every `selector { body }` pair, with comments stripped. A rule inside a media
 * query comes back like any other — that is what lets the narrow-screen
 * override be checked next to the base rule it overrides.
 */
function rules(source: string): Rule[] {
  const found: Rule[] = [];
  const pattern = /([^{}]+)\{([^{}]*)\}/g;
  const text = source.replace(/\/\*[\s\S]*?\*\//g, "");
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    found.push({ selector: match[1].trim(), body: match[2] });
  }
  return found;
}

const all = rules(css);
const selectorIs = (selector: string) => all.filter((rule) => rule.selector === selector);

describe("scroll containment in style.css", () => {
  it("parses the stylesheet, so the checks below are not vacuous", () => {
    expect(all.length).toBeGreaterThan(300);
    expect(selectorIs(".modal").length).toBeGreaterThan(0);
    expect(selectorIs(".modal-backdrop > .modal").length).toBeGreaterThan(0);
  });

  it("caps and scrolls the dialog the backdrop centres", () => {
    const base = selectorIs(".modal-backdrop > .modal")[0];
    expect(base.body).toContain("overflow-y: auto");
    expect(base.body).toContain("max-height: calc(100vh - 40px)");
    expect(base.body).toContain("max-height: calc(100dvh - 40px)");
  });

  it("leaves a bare .modal rule without a height cap or a scrollbar", () => {
    // A bare `.modal` selector reaches `.inline-modal` too — every inline form
    // is written `class="modal inline-modal"` — so an overflow or a max-height
    // here lands on the cards in the page flow. The loop covers both the base
    // rule and the narrow-screen one, which is the pair that has to stay clean.
    for (const rule of selectorIs(".modal")) {
      expect(rule.body, `.modal { ${rule.body.trim()} }`).not.toContain("overflow");
      expect(rule.body, `.modal { ${rule.body.trim()} }`).not.toContain("max-height");
    }
  });

  it("keeps the narrow-screen cap on the dialog, not on every .modal", () => {
    // Written without the backdrop prefix this rule would re-cap the inline
    // cards below 520px: the original bug, back again, but only on phones.
    const override = all.find(
      (rule) => rule.selector === ".modal-backdrop > .modal" && rule.body.includes("100dvh - 20px"),
    );
    expect(override, "the narrow-screen cap has to stay scoped to the backdrop dialog").toBeDefined();
  });

  it("gives an inline card no scroll container of its own", () => {
    const inlineRules = all.filter((rule) => rule.selector.includes(".inline-modal"));
    expect(inlineRules.length).toBeGreaterThan(0);
    for (const rule of inlineRules) {
      expect(rule.body, `${rule.selector} { ... }`).not.toContain("overflow");
    }
  });

  it("stops the wheel at the run-log dialog, not at the log cell", () => {
    // The dialog's only nested scroller is the capped log cell, and a notch it
    // cannot use used to chain past the backdrop and move the page behind the
    // dialog. Containment on the backdrop child is what cuts that chain, and it
    // works because `overflow: hidden` already makes the box a scroll container
    // — containment needs that identity, not room to scroll. The two properties
    // therefore have to stay on one rule: take the `overflow` away and the
    // `overscroll-behavior` beside it quietly stops doing anything. It cannot
    // move down to `.runs-table .run-log` instead — a log that already fits has
    // nothing inside to scroll and would swallow the wheel, turning the widest
    // column of the table into a dead zone.
    const edge = selectorIs(".modal-backdrop > .modal-runlog");
    expect(edge).toHaveLength(1);
    expect(edge[0].body).toContain("overflow: hidden");
    expect(edge[0].body).toContain("overscroll-behavior: contain");

    const capped = selectorIs(".runs-table .run-log");
    expect(capped).toHaveLength(1);
    expect(capped[0].body).toContain("overflow-y: auto");
    expect(capped[0].body).not.toContain("overscroll-behavior");

    // The per-cell class the wheel handler used to drive is gone with it.
    expect(all.some((rule) => rule.selector.includes("run-log-can-scroll"))).toBe(false);
  });

  it("keeps the one scrollbar the action form is allowed to have", () => {
    // The 220px task picker: it is what stops a long task list from stretching
    // the page, and it is the only scrolling the form should show.
    const boxes = selectorIs(".check-list");
    expect(boxes).toHaveLength(1);
    expect(boxes[0].body).toContain("max-height: 220px");
    expect(boxes[0].body).toContain("overflow-y: auto");
  });
});

describe("quiet blue console theme", () => {
  const themes = [
    all.find((rule) => rule.selector.includes('[data-theme="light"]'))!,
    selectorIs('[data-theme="dark"]')[0],
  ];

  function token(body: string, name: string): string {
    const value = body.match(new RegExp(`--${name}:\\s*([^;]+);`))?.[1];
    if (!value) throw new Error(`Missing theme token: ${name}`);
    return value.trim();
  }

  function luminance(colour: string): number {
    const channels = colour.startsWith("#")
      ? colour.slice(1).match(/../g)!.map((part) => parseInt(part, 16))
      : colour.split(/\s+/).map(Number);
    return channels.map((channel) => {
      const value = channel / 255;
      return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    }).reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  }

  function contrast(a: string, b: string): number {
    const first = luminance(a);
    const second = luminance(b);
    return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
  }

  it("keeps small text readable on cards, the page and the sidebar in both themes", () => {
    for (const theme of themes) {
      for (const surface of ["page-surface", "card-surface", "panel-solid-surface"]) {
        for (const text of ["ink", "ink-2", "ink-3", "accent", "ok", "warn", "bad"]) {
          expect(contrast(token(theme.body, text), token(theme.body, surface)),
            `${theme.selector}: ${text} on ${surface}`).toBeGreaterThanOrEqual(4.5);
        }
      }
    }
  });

  it("keeps primary button labels readable at rest and on hover", () => {
    for (const theme of themes) {
      for (const background of ["primary", "primary-hover"]) {
        expect(contrast(token(theme.body, "primary-content"), token(theme.body, background)))
          .toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("uses independent outlined statistic cards and shared shell dimensions", () => {
    expect(selectorIs(".stats > div")[0].body).toContain("border: 1px solid var(--line)");
    expect(selectorIs(".sidebar")[0].body).toContain("width: var(--sidebar-width)");
    expect(selectorIs(".app-main")[0].body).toContain("margin-left: var(--sidebar-width)");
    expect(selectorIs(".topbar")[0].body).toContain("height: var(--topbar-height)");
  });

  it("keeps the first-paint browser colour aligned with each page surface", () => {
    const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
    const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
    for (const theme of themes) {
      const hex = `#${token(theme.body, "page-surface").split(/\s+/)
        .map((part) => Number(part).toString(16).padStart(2, "0")).join("")}`;
      expect(html).toContain(hex);
      expect(app).toContain(hex);
    }
  });
});
