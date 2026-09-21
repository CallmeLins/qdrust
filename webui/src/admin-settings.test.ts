import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The admin settings form talks to a generic key/value API: the server stores
 * whatever key it is given and only reacts to the ones it knows. A misspelt
 * key is therefore accepted, persisted, and ignored — the switch renders, saves
 * and reports success, and nothing happens. That failure mode is invisible from
 * either side alone, so the names are pinned across the two languages here.
 *
 * The switches are described as data rather than written out twice: both halves
 * of ADR-0008 follow the same contract, and a third relaxation should be a row
 * in this table rather than a fourth copy of the same six assertions.
 *
 * Structural assertions on App.vue, like pagination.test.ts: this repo has no
 * @vue/test-utils and does not add a dependency for one form.
 */
const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
const api = readFileSync(new URL("../../crates/qdrust-server/src/api.rs", import.meta.url), "utf8");
const scheduler = readFileSync(
  new URL("../../crates/qdrust-server/src/scheduler.rs", import.meta.url),
  "utf8",
);
const i18n = readFileSync(new URL("./i18n.ts", import.meta.url), "utf8");
const css = readFileSync(new URL("./style.css", import.meta.url), "utf8");

/** The value of `const NAME: &str = "...";` in the Rust source. */
function rustConst(source: string, name: string): string {
  const pattern = new RegExp(`const\\s+${name}\\s*:\\s*&str\\s*=\\s*"([^"]+)"`);
  const found = source.match(pattern);
  expect(found, `${name} must exist in api.rs`).not.toBeNull();
  return found![1];
}

/** One ADR-0008 relaxation, as the four names it carries in each language. */
const SWITCHES = [
  {
    name: "private-network",
    rustConst: "ALLOW_PRIVATE_NETWORK_SETTING",
    vueConst: "ALLOW_PRIVATE_NETWORK_KEY",
    key: "security.allow_private_network",
    field: "allowPrivateNetwork",
    loadVar: "privateNetwork",
    label: "allowPrivateNetwork",
    risk: "allowPrivateNetworkRisk",
  },
  {
    name: "invalid-certificates",
    rustConst: "ALLOW_INVALID_CERTIFICATES_SETTING",
    vueConst: "ALLOW_INVALID_CERTIFICATES_KEY",
    key: "security.allow_invalid_certificates",
    field: "allowInvalidCertificates",
    loadVar: "invalidCertificates",
    label: "allowInvalidCertificates",
    risk: "allowInvalidCertificatesRisk",
  },
] as const;

/** `ALLOW_PRIVATE_NETWORK_SETTING` -> `allow_private_network`. */
function rustField(rustConstName: string): string {
  return rustConstName.replace(/_SETTING$/, "").toLowerCase();
}

for (const sw of SWITCHES) {
  describe(`${sw.name} switch`, () => {
    const field = rustField(sw.rustConst);

    it("matches the constant the server actually applies", () => {
      // Read, not hardcoded: a rename on either side has to be a rename on both.
      expect(rustConst(api, sw.rustConst)).toBe(sw.key);
      expect(app).toContain(`const ${sw.vueConst} = "${sw.key}"`);
    });

    it("is the key the server routes into the executor policy", () => {
      // Both ends of the chain. The const must be a match arm in the mapping
      // table, its arm must write the runtime field, and the scheduler must take
      // that field through to the executor — the original bug was a flag that
      // existed on both sides with nothing joining them.
      expect(api).toContain(`${sw.rustConst} => {`);
      expect(api).toContain(`runtime.${field} = v`);
      expect(scheduler).toContain(`${field}: policy.${field},`);
    });

    it("loads the stored value instead of defaulting every visit", () => {
      expect(app).toContain(`s.key === ${sw.vueConst}`);
      expect(app).toContain(`settingsForm.${sw.field} = ${sw.loadVar}?.value === true`);
    });

    it("binds the checkbox and saves it back", () => {
      // The exact save line, not just the presence of the key: two switches
      // wired from one form field is precisely the copy-paste this catches.
      expect(app).toContain(`v-model="settingsForm.${sw.field}" type="checkbox"`);
      expect(app).toContain(`api.adminSetSetting(${sw.vueConst}, settingsForm.${sw.field})`);
    });

    it("warns that the switch is high risk", () => {
      // ADR-0008: each opt-in has to be marked as high risk in the UI, not just
      // granted. A bare checkbox would understate what it unlocks.
      expect(app).toContain(`t('${sw.label}')`);
      expect(app).toContain(`t('${sw.risk}')`);
    });

    it("defines both labels in Chinese and English", () => {
      for (const key of [`${sw.label}:`, `${sw.risk}:`]) {
        const occurrences = i18n.split(`  ${key}`).length - 1;
        expect(occurrences, `${key} must appear in zh and en`).toBe(2);
      }
    });
  });
}

describe("the two switches", () => {
  it("are independent names, fields and keys", () => {
    // "Grant them separately" is the ADR's wording, so nothing may be shared
    // between the columns of the table above. This is the assertion that fails
    // first if someone collapses the pair into one switch while wiring.
    for (const property of ["key", "field", "label", "risk", "rustConst", "vueConst"] as const) {
      const values = SWITCHES.map((sw) => sw[property]);
      expect(new Set(values).size, `${property} must differ between switches`).toBe(values.length);
    }
  });

  it("each carry their own high-risk notice, spanning both columns", () => {
    // A checkbox added without its notice would leave the form looking complete
    // while one relaxation goes unflagged.
    expect(app.split('class="risk-notice"').length - 1).toBe(SWITCHES.length);
    const rule = css.match(/\.risk-notice\s*\{[^}]*\}/)?.[0] ?? "";
    expect(rule, "style.css must style .risk-notice").not.toBe("");
    expect(rule).toContain("grid-column: 1 / -1");
  });
});
