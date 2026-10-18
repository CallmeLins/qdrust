export type AvatarSpec = {
  id: string;
  name: string;
  label: string;
  background: string;
  accent: string;
  ink: string;
  face: string;
  detail: string;
};

/**
 * Built-in avatars only. Each drawing is local SVG, so selecting an avatar
 * never turns into an image upload, a remote request, or stored user content.
 */
export const AVATARS: AvatarSpec[] = [
  {
    id: "avatar-01",
    name: "太阳",
    label: "Sunrise",
    background: "#F5D9B8",
    accent: "#E9A35B",
    ink: "#7A4D2B",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail:
      "M32 13v5M32 46v5M13 32h5M46 32h5M18 18l4 4M42 42l4 4M46 18l-4 4M22 42l-4 4",
  },
  {
    id: "avatar-02",
    name: "薄荷",
    label: "Mint",
    background: "#CFE8D6",
    accent: "#69B68B",
    ink: "#2D6347",
    face: "M32 21a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail:
      "M20 25c2-8 8-12 12-12s10 4 12 12M23 39c3 6 6 7 9 7s6-1 9-7",
  },
  {
    id: "avatar-03",
    name: "海风",
    label: "Ocean",
    background: "#CDE3EF",
    accent: "#5D9FC3",
    ink: "#28566F",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail:
      "M18 38c4-4 8-4 12 0s8 4 12 0 4-4 8 0M18 45c4-4 8-4 12 0s8 4 12 0 4-4 8 0",
  },
  {
    id: "avatar-04",
    name: "晚霞",
    label: "Dusk",
    background: "#E9D5DF",
    accent: "#BF7C9B",
    ink: "#6E3D54",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail: "M18 22l5 5M46 22l-5 5M20 42c4-6 8-8 12-8s8 2 12 8",
  },
  {
    id: "avatar-05",
    name: "麦田",
    label: "Field",
    background: "#EEE1B9",
    accent: "#C5A04F",
    ink: "#645025",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail: "M16 44h32M20 39l4-8 4 8M36 39l4-8 4 8",
  },
  {
    id: "avatar-06",
    name: "松针",
    label: "Pine",
    background: "#D5E5D2",
    accent: "#7FA06E",
    ink: "#3E5634",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail: "M32 13l-9 14h6l-8 12h22l-8-12h6Z",
  },
  {
    id: "avatar-07",
    name: "云朵",
    label: "Cloud",
    background: "#DDE4EE",
    accent: "#8EA6C1",
    ink: "#485B70",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail:
      "M17 43c-4 0-6-2-6-5s3-5 6-5c1-5 5-8 10-8 6 0 10 4 10 9 4 0 7 2 7 5s-3 5-7 5Z",
  },
  {
    id: "avatar-08",
    name: "星轨",
    label: "Star",
    background: "#E7DDF0",
    accent: "#A282C2",
    ink: "#57416E",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail:
      "M46 15l2 5 5 2-5 2-2 5-2-5-5-2 5-2ZM16 36l1 3 3 1-3 1-1 3-1-3-3-1 3-1Z",
  },
  {
    id: "avatar-09",
    name: "陶土",
    label: "Clay",
    background: "#EFD7CC",
    accent: "#C67E62",
    ink: "#764432",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail: "M20 18c4 3 8 4 12 4s8-1 12-4M22 42c3 5 6 7 10 7s7-2 10-7",
  },
  {
    id: "avatar-10",
    name: "墨石",
    label: "Stone",
    background: "#D8DEE2",
    accent: "#8999A2",
    ink: "#415058",
    face: "M32 20a12 12 0 1 0 0 24 12 12 0 0 0 0-24Z",
    detail: "M16 45l7-13 6 8 5-11 5 16M16 45h32",
  },
];

export function avatarName(locale: "zh-CN" | "en-US", avatar: AvatarSpec): string {
  return locale === "zh-CN" ? avatar.name : avatar.label;
}
