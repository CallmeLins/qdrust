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

  it("keeps the one scrollbar the action form is allowed to have", () => {
    // The 220px task picker: it is what stops a long task list from stretching
    // the page, and it is the only scrolling the form should show.
    const boxes = selectorIs(".check-list");
    expect(boxes).toHaveLength(1);
    expect(boxes[0].body).toContain("max-height: 220px");
    expect(boxes[0].body).toContain("overflow-y: auto");
  });
});
