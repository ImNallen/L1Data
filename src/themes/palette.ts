import type { Appearance } from "../ipc/bindings";
import { contrast, formatHex, mix, parseHex, type Rgba } from "./color";

export const TOKENS = {
  background: "--color-bg",
  sidebar: "--color-sidebar",
  canvas: "--color-canvas",
  chatSurface: "--chat-surface",
  chatBorder: "--chat-border",
  chatSelected: "--chat-selected",
  chatControl: "--chat-control",
  chatKeyword: "--chat-keyword",
  chatString: "--chat-string",
  raised: "--color-raised",
  hover: "--color-hover",
  selected: "--color-selected",
  pill: "--color-pill",
  border: "--color-border",
  grid: "--color-grid",
  control: "--color-control",
  foreground: "--color-fg",
  tree: "--color-tree",
  muted: "--color-muted",
  dim: "--color-dim",
  faint: "--color-faint",
  green: "--color-green",
  live: "--color-live",
  amber: "--color-amber",
  keyword: "--color-keyword",
  badge: "--color-badge",
  badgeBackground: "--color-badge-bg",
  accent: "--color-run",
  accentHover: "--color-run-hover",
  accentForeground: "--color-run-fg",
  danger: "--color-danger",
  cellSelect: "--color-cell-select",
  runIcon: "--color-run-icon",
  focus: "--color-focus",
  accentBorder: "--color-accent-border",
  handleHover: "--color-handle-hover",
  backdrop: "--color-backdrop",
  segmentTrack: "--color-segment-track",
  segmentThumb: "--color-segment-thumb",
  segmentThumbBorder: "--color-segment-thumb-border",
  scrollbar: "--scrollbar",
  scrollbarHover: "--scrollbar-hover",
  code: "--code",
  codePunct: "--code-punct",
  codeSelection: "--code-selection",
  codeBracket: "--code-bracket",
  codeSelectionMatch: "--code-selection-match",
  codeSearchMatch: "--code-search-match",
  codeSearchCurrent: "--code-search-current",
  codeExecuted: "--code-executed",
  codeRunTarget: "--code-run-target",
  codeRunTargetBorder: "--code-run-target-border",
} as const;

export type Token = keyof typeof TOKENS;
export type Palette = Partial<Record<Token, string>>;

export const TOKEN_NAMES = Object.keys(TOKENS).filter(isToken);

export function isToken(key: string): key is Token {
  return Object.hasOwn(TOKENS, key);
}

type Seeds = { background: Rgba; foreground: Rgba; accent: Rgba };
type Recipe = (s: Seeds) => Rgba;

const WHITE: Rgba = { r: 1, g: 1, b: 1, a: 1 };
const BLACK: Rgba = { r: 0, g: 0, b: 0, a: 1 };

const ink =
  (t: number): Recipe =>
  (s) =>
    mix(s.background, s.foreground, t);
const tint =
  (t: number): Recipe =>
  (s) =>
    mix(s.background, s.accent, t);
const lift =
  (t: number): Recipe =>
  (s) =>
    mix(s.accent, s.foreground, t);
const shade =
  (t: number): Recipe =>
  (s) =>
    mix(s.background, BLACK, t);
const glaze =
  (alpha: number): Recipe =>
  (s) => ({ ...s.accent, a: alpha });
const fixed = (hex: string): Recipe => {
  const color = parseHex(hex) ?? BLACK;
  return () => color;
};
// Labels on the accent are 12-13px, so they need body-text contrast; white wins ties to keep the usual look.
const onAccent: Recipe = (s) => {
  const dark = mix(s.accent, BLACK, 0.85);
  const white = contrast(WHITE, s.accent);
  return white >= 4.5 || white >= contrast(dark, s.accent) ? WHITE : dark;
};
// The focus ring must stay visible (3:1) against the background whatever the seeds, so a pale result is pulled toward the foreground.
const visible =
  (recipe: Recipe): Recipe =>
  (s) => {
    const base = recipe(s);
    let color = base;
    for (let t = 0.1; t <= 1 && contrast(color, s.background) < 3; t += 0.1) color = mix(base, s.foreground, t);
    return color;
  };

const seed =
  (key: keyof Seeds): Recipe =>
  (s) =>
    s[key];

// Near-white or near-black, keeping a trace of the background's hue.
const FOREGROUND: Record<Appearance, (background: Rgba) => Rgba> = {
  dark: (bg) => mix(bg, WHITE, 0.94),
  light: (bg) => mix(bg, BLACK, 0.82),
};

// Ratios are fitted to L1's own stylesheet, so an L1-like seed derives an L1-like palette.
const RECIPES: Record<Appearance, Record<Token, Recipe>> = {
  dark: {
    background: seed("background"),
    accent: seed("accent"),
    foreground: seed("foreground"),
    sidebar: shade(0.3),
    canvas: ink(0.043),
    chatSurface: ink(0.064),
    chatBorder: ink(0.14),
    chatSelected: ink(0.115),
    chatControl: ink(0.14),
    chatKeyword: lift(0.55),
    chatString: fixed("#96b2a0"),
    raised: ink(0.04),
    hover: ink(0.058),
    selected: ink(0.093),
    pill: ink(0.093),
    border: ink(0.078),
    grid: ink(0.093),
    control: ink(0.105),
    tree: ink(0.98),
    muted: ink(0.692),
    dim: ink(0.555),
    faint: ink(0.4),
    green: fixed("#00d492"),
    live: fixed("#00bc7d"),
    amber: fixed("#ffb900"),
    danger: fixed("#ff6467"),
    keyword: lift(0.233),
    badge: lift(0.293),
    badgeBackground: tint(0.133),
    accentHover: lift(0.113),
    accentForeground: onAccent,
    cellSelect: tint(0.243),
    runIcon: onAccent,
    focus: visible(lift(0.15)),
    accentBorder: tint(0.557),
    handleHover: ink(0.32),
    backdrop: ink(0.115),
    segmentTrack: ink(0.058),
    segmentThumb: ink(0.14),
    segmentThumbBorder: ink(0.18),
    scrollbar: ink(0.15),
    scrollbarHover: ink(0.213),
    code: ink(0.942),
    codePunct: ink(0.877),
    codeSelection: tint(0.383),
    codeBracket: ink(0.15),
    codeSelectionMatch: fixed("#99ff7740"),
    codeSearchMatch: fixed("#ffb9002e"),
    codeSearchCurrent: fixed("#ffb90066"),
    codeExecuted: glaze(0.16),
    codeRunTarget: glaze(0.15),
    codeRunTargetBorder: glaze(0.35),
  },
  light: {
    background: seed("background"),
    accent: seed("accent"),
    foreground: seed("foreground"),
    sidebar: ink(0.03),
    canvas: ink(0.018),
    chatSurface: seed("background"),
    chatBorder: ink(0.115),
    chatSelected: ink(0.06),
    chatControl: ink(0.065),
    chatKeyword: lift(0.5),
    chatString: fixed("#567b64"),
    raised: ink(0),
    hover: ink(0.055),
    selected: ink(0.083),
    pill: ink(0.068),
    border: ink(0.093),
    grid: ink(0.07),
    control: ink(0.13),
    tree: ink(0.957),
    muted: ink(0.732),
    dim: ink(0.605),
    faint: ink(0.435),
    green: fixed("#007f55"),
    live: fixed("#00a36c"),
    amber: fixed("#a65800"),
    danger: fixed("#d03539"),
    keyword: lift(0.108),
    badge: lift(0.12),
    badgeBackground: tint(0.113),
    accentHover: lift(0.085),
    accentForeground: onAccent,
    cellSelect: tint(0.165),
    runIcon: onAccent,
    focus: visible(tint(0.8)),
    accentBorder: tint(0.652),
    handleHover: ink(0.363),
    backdrop: ink(0.363),
    segmentTrack: ink(0.07),
    segmentThumb: ink(0),
    segmentThumbBorder: ink(0.12),
    scrollbar: ink(0.158),
    scrollbarHover: ink(0.28),
    code: ink(0.95),
    codePunct: ink(0.732),
    codeSelection: tint(0.225),
    codeBracket: ink(0.105),
    codeSelectionMatch: fixed("#00996629"),
    codeSearchMatch: fixed("#ffb9004d"),
    codeSearchCurrent: fixed("#ffa0008c"),
    codeExecuted: glaze(0.1),
    codeRunTarget: glaze(0.11),
    codeRunTargetBorder: glaze(0.3),
  },
};

function mapRecord<K extends string, A, B>(record: Record<K, A>, f: (a: A) => B): Record<K, B> {
  const out = {} as Record<K, B>;
  for (const key in record) out[key] = f(record[key]);
  return out;
}

/**
 * Every token from two seed colours, and optionally a foreground to derive text and surfaces toward.
 * Callers pass validated hex; an unparsable seed reads as black.
 */
export function derivePalette(appearance: Appearance, background: string, accent: string, foreground?: string): Record<Token, string> {
  const bg = parseHex(background) ?? BLACK;
  const fg = (foreground && parseHex(foreground)) || FOREGROUND[appearance](bg);
  const seeds = { background: bg, accent: parseHex(accent) ?? BLACK, foreground: fg };
  return mapRecord(RECIPES[appearance], (recipe) => formatHex(recipe(seeds)));
}

export type ThemeColors = Palette & { background: string; accent: string };

export function completePalette(appearance: Appearance, colors: ThemeColors): Record<Token, string> {
  return { ...derivePalette(appearance, colors.background, colors.accent, colors.foreground), ...colors };
}
