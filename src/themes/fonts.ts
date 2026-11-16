import { isMac } from "../shell/hostedWindow";

export type FontKind = "interface" | "code";

/**
 * `stack` is the CSS font-family list painted for the stored `family` name.
 * `probe` names the face to look for on this machine; null means always offered (bundled or generic).
 */
export type FontEntry = { family: string; stack: string; probe: string | null };

const FALLBACK: Record<FontKind, string> = {
  interface: "ui-sans-serif, system-ui, sans-serif",
  code: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
};

const APPLE = typeof navigator !== "undefined" && isMac(navigator.userAgent);

const installed = (kind: FontKind, family: string): FontEntry => ({ family, stack: `"${family}", ${FALLBACK[kind]}`, probe: `"${family}"` });

const FONTS: Record<FontKind, readonly FontEntry[]> = {
  interface: [
    { family: "Inter", stack: `"Inter Variable", ${FALLBACK.interface}`, probe: null },
    { family: "System", stack: 'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif', probe: null },
    installed("interface", "Helvetica Neue"),
    installed("interface", "Avenir Next"),
    installed("interface", "Segoe UI"),
    installed("interface", "Roboto"),
    installed("interface", "IBM Plex Sans"),
  ],
  code: [
    { family: "JetBrains Mono", stack: `"JetBrains Mono Variable", ${FALLBACK.code}`, probe: null },
    // WKWebView exposes SF Mono only through the ui-monospace keyword, which other engines resolve to another face or ignore.
    {
      family: "SF Mono",
      stack: '"SF Mono", SFMono-Regular, ui-monospace, Menlo, monospace',
      probe: APPLE ? 'ui-monospace, "SF Mono", SFMono-Regular' : '"SF Mono", SFMono-Regular',
    },
    installed("code", "Menlo"),
    installed("code", "Monaco"),
    installed("code", "Fira Code"),
    installed("code", "Cascadia Code"),
    installed("code", "Source Code Pro"),
    installed("code", "IBM Plex Mono"),
    installed("code", "Consolas"),
    installed("code", "Courier New"),
  ],
};

export const FONT_SIZES: Record<FontKind, readonly number[]> = {
  interface: [11, 12, 13, 14, 15, 16],
  code: [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20],
};

export function fontStack(kind: FontKind, family: string): string {
  return FONTS[kind].find((f) => f.family === family)?.stack ?? `"${family}", ${FALLBACK[kind]}`;
}

export function fontChoices(kind: FontKind, saved: string, isInstalled: (probe: string) => boolean): string[] {
  const offered = FONTS[kind].filter((f) => f.probe === null || f.family === saved || isInstalled(f.probe)).map((f) => f.family);
  return offered.includes(saved) ? offered : [...offered, saved];
}

const SAMPLE = "mmmmmmmmmmlli10OQ@#";
const GENERICS = ["monospace", "sans-serif", "serif"];

/**
 * Canvas width probing: a face is present when putting it ahead of a generic family changes the text width.
 * `document.queryLocalFonts` would be exact but WKWebView does not implement it.
 */
export function canvasFontProbe(): (probe: string) => boolean {
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return () => false;
  const width = (font: string) => {
    ctx.font = `32px ${font}`;
    return ctx.measureText(SAMPLE).width;
  };
  const base = GENERICS.map(width);
  const seen = new Map<string, boolean>();
  return (probe) => {
    let hit = seen.get(probe);
    if (hit === undefined) {
      hit = GENERICS.some((g, i) => width(`${probe}, ${g}`) !== base[i]);
      seen.set(probe, hit);
    }
    return hit;
  };
}
