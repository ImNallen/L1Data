import type { Appearance, ColorScheme, CustomTheme, CustomThemeVariant, Settings } from "../ipc/bindings";
import { isHex } from "./color";
import { completePalette, derivePalette, isToken, TOKEN_NAMES, type Palette, type ThemeColors } from "./palette";

type ThemeId = string;

export type ThemeVariant = {
  id: ThemeId;
  name: string;
  appearance: Appearance;
  swatch: { background: string; accent: string };
  palette: Palette;
};

export type ThemeFamily = {
  id: string;
  name: string;
  /** Light variants first, then dark, each in source order. */
  variants: readonly ThemeVariant[];
  custom: boolean;
};

export type ThemeDraft = { name: string; appearance: Appearance; colors: ThemeColors };

export type ThemeFamilyDraft = { name: string; variants: readonly ThemeDraft[] };

export const APPEARANCES = ["light", "dark"] as const satisfies readonly Appearance[];

export const APPEARANCE_LABEL: Record<Appearance, string> = { light: "Light", dark: "Dark" };

// index.css is L1's palette, so painting L1 sets no inline vars.
export const L1_VARIANTS: Record<Appearance, ThemeVariant> = {
  light: { id: "l1-light", name: "L1 Light", appearance: "light", swatch: { background: "#ffffff", accent: "#346bf1" }, palette: {} },
  dark: { id: "l1-dark", name: "L1 Dark", appearance: "dark", swatch: { background: "#0a0a0a", accent: "#346bf1" }, palette: {} },
};

const L1: ThemeFamily = { id: "l1", name: "L1", custom: false, variants: [L1_VARIANTS.light, L1_VARIANTS.dark] };

type Seed = { background: string; accent: string };

function derivedFamily(id: string, name: string, seeds: Record<Appearance, Seed>): ThemeFamily {
  const variant = (appearance: Appearance): ThemeVariant => ({
    id: `${id}-${appearance}`,
    name: `${name} ${APPEARANCE_LABEL[appearance]}`,
    appearance,
    swatch: seeds[appearance],
    palette: derivePalette(appearance, seeds[appearance].background, seeds[appearance].accent),
  });
  return { id, name, custom: false, variants: [variant("light"), variant("dark")] };
}

export const BUILT_IN_FAMILIES: readonly ThemeFamily[] = [
  L1,
  derivedFamily("grove", "Grove", { light: { background: "#fafcf8", accent: "#23845a" }, dark: { background: "#0c1310", accent: "#2f9e6e" } }),
  derivedFamily("ocean", "Ocean", { light: { background: "#f6f9fc", accent: "#1477b0" }, dark: { background: "#0a1219", accent: "#2b8fc7" } }),
  derivedFamily("ember", "Ember", { light: { background: "#fdf9f6", accent: "#c75a1a" }, dark: { background: "#14100d", accent: "#e0702a" } }),
  derivedFamily("iris", "Iris", { light: { background: "#f9f8fe", accent: "#6a48f0" }, dark: { background: "#0f0e17", accent: "#7c5cff" } }),
  derivedFamily("rose", "Rose", { light: { background: "#fdf7f9", accent: "#cc2f66" }, dark: { background: "#150d11", accent: "#e0457b" } }),
];

/** Settings come from disk, so colours are checked here: unknown tokens and non-hex values are dropped. */
export function customVariant(theme: CustomThemeVariant): ThemeVariant {
  const colors: Palette = {};
  for (const [key, value] of Object.entries(theme.colors)) if (isToken(key) && isHex(value)) colors[key] = value.trim();
  const fallback = L1_VARIANTS[theme.appearance].swatch;
  const swatch = { background: colors.background ?? fallback.background, accent: colors.accent ?? fallback.accent };
  return {
    id: theme.id,
    name: theme.name,
    appearance: theme.appearance,
    swatch,
    palette: completePalette(theme.appearance, { ...colors, ...swatch }),
  };
}

/** A draft that reproduces `variant`, keeping only the colours its seeds don't already derive. */
export function draftOf(variant: ThemeVariant, name: string): ThemeDraft {
  const { appearance, swatch, palette } = variant;
  const base = derivePalette(appearance, swatch.background, swatch.accent);
  const foreground = palette.foreground !== base.foreground ? palette.foreground : undefined;
  const derived = foreground ? derivePalette(appearance, swatch.background, swatch.accent, foreground) : base;
  const colors: ThemeColors = { ...swatch };
  for (const token of TOKEN_NAMES) {
    const value = palette[token];
    if (value && value !== derived[token]) colors[token] = value;
  }
  return { name, appearance, colors };
}

/** The card a stored theme shows as, or null when it has no variants to show. */
export function customFamily(theme: CustomTheme): ThemeFamily | null {
  const parsed = theme.variants.map(customVariant);
  const variants = APPEARANCES.flatMap((a) => parsed.filter((v) => v.appearance === a));
  return variants.length > 0 ? { id: theme.id, name: theme.name, custom: true, variants } : null;
}

export function themeFamilies(customThemes: readonly CustomTheme[]): ThemeFamily[] {
  return [...BUILT_IN_FAMILIES, ...customThemes.flatMap((theme) => customFamily(theme) ?? [])];
}

/** The family's first light and first dark variant: what picking the family as a whole sets. */
export function familyPicks(family: ThemeFamily): ThemeVariant[] {
  return APPEARANCES.flatMap((a) => family.variants.find((v) => v.appearance === a) ?? []);
}

/** The appearance a scheme pins, or null while it follows the system. */
export const FIXED_APPEARANCE: Record<ColorScheme, Appearance | null> = { System: null, Light: "light", Dark: "dark" };
export const SCHEME_FOR: Record<Appearance, ColorScheme> = { light: "Light", dark: "Dark" };

function schemeAppearance(scheme: ColorScheme, systemPrefersDark: boolean): Appearance {
  return FIXED_APPEARANCE[scheme] ?? (systemPrefersDark ? "dark" : "light");
}

export type ThemeSettings = Pick<Settings, "colorScheme" | "lightTheme" | "darkTheme" | "customThemes">;

export const THEME_SLOT = { light: "lightTheme", dark: "darkTheme" } as const satisfies Record<Appearance, keyof Settings>;

/** The variant saved for `appearance`, or L1's when the saved id is gone or belongs to the other appearance. */
export function pickedVariant(settings: ThemeSettings, appearance: Appearance): ThemeVariant {
  const id = settings[THEME_SLOT[appearance]];
  const variants = themeFamilies(settings.customThemes).flatMap((f) => f.variants);
  return variants.find((v) => v.id === id && v.appearance === appearance) ?? L1_VARIANTS[appearance];
}

export function resolveTheme(settings: ThemeSettings, systemPrefersDark: boolean): ThemeVariant {
  return pickedVariant(settings, schemeAppearance(settings.colorScheme, systemPrefersDark));
}
