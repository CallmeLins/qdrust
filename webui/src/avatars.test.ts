import { describe, expect, it } from "vitest";
import { AVATARS, avatarName } from "./avatars";

describe("built-in avatars", () => {
  it("offers ten stable, unique choices", () => {
    expect(AVATARS).toHaveLength(10);
    expect(new Set(AVATARS.map((avatar) => avatar.id)).size).toBe(10);
    expect(AVATARS.map((avatar) => avatar.id)).toEqual([
      "avatar-01",
      "avatar-02",
      "avatar-03",
      "avatar-04",
      "avatar-05",
      "avatar-06",
      "avatar-07",
      "avatar-08",
      "avatar-09",
      "avatar-10",
    ]);
  });

  it("labels each avatar in both supported locales", () => {
    for (const avatar of AVATARS) {
      expect(avatarName("zh-CN", avatar)).toBe(avatar.name);
      expect(avatarName("en-US", avatar)).toBe(avatar.label);
    }
  });
});
