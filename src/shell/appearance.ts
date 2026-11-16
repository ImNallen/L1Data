import { useSyncExternalStore } from "react";
import type { Density, ReduceMotion, Settings } from "../ipc/bindings";
import { useWorkspace } from "../state/workspace";
import { fontStack } from "../themes/fonts";

export type AppearanceSettings = Pick<Settings, "interfaceFont" | "codeFont" | "density" | "panelMotionMs">;

export const DENSITY_FACTOR: Record<Density, number> = { Compact: 0.85, Standard: 1, Spacious: 1.15 };

/** The design is drawn at 13 px; every interface text size scales by `size / 13`. */
const BASE_UI_SIZE = 13;

/** Result grid row height; 13 px code at Standard density gives the original 24 px. */
export function gridRowHeight(codeSize: number, density: Density): number {
  return Math.round((codeSize + 11) * DENSITY_FACTOR[density]);
}

export function appearanceVars(s: AppearanceSettings): Record<string, string> {
  return {
    "--ui-font": fontStack("interface", s.interfaceFont.family),
    "--ui-scale": String(s.interfaceFont.size / BASE_UI_SIZE),
    "--code-font": fontStack("code", s.codeFont.family),
    "--code-size": `${s.codeFont.size}px`,
    "--code-line-height": `${Math.round(s.codeFont.size * 1.54)}px`,
    "--density": String(DENSITY_FACTOR[s.density]),
    "--panel-duration": `${s.panelMotionMs}ms`,
  };
}

const changed = (s: AppearanceSettings, prev: AppearanceSettings) =>
  s.interfaceFont !== prev.interfaceFont || s.codeFont !== prev.codeFont || s.density !== prev.density || s.panelMotionMs !== prev.panelMotionMs;

function paintAppearance(s: AppearanceSettings): void {
  const root = document.documentElement.style;
  for (const [name, value] of Object.entries(appearanceVars(s))) root.setProperty(name, value);
}

const REDUCED_QUERY = "(prefers-reduced-motion: reduce)";

/** `System` follows the operating system's reduce-motion accessibility setting. */
export const motionReduced = (setting: ReduceMotion, systemReduced: boolean) => setting === "On" || (setting === "System" && systemReduced);

const systemReduced = () => window.matchMedia(REDUCED_QUERY).matches;
const subscribeSystem = (onChange: () => void) => {
  const media = window.matchMedia(REDUCED_QUERY);
  media.addEventListener("change", onChange);
  return () => media.removeEventListener("change", onChange);
};

/** Whether motion is reduced right now, for code that drives motion itself instead of through the stylesheet. */
export function useReducedMotion(): boolean {
  const setting = useWorkspace((s) => s.settings.reduceMotion);
  const system = useSyncExternalStore(subscribeSystem, systemReduced, () => false);
  return motionReduced(setting, system);
}

/** `data-motion="reduced"` on the root switches the stylesheet's reduced-motion rules on. */
function paintMotion(setting: ReduceMotion): void {
  const { dataset } = document.documentElement;
  if (motionReduced(setting, systemReduced())) dataset.motion = "reduced";
  else delete dataset.motion;
}

export function startAppearance(): () => void {
  const { settings } = useWorkspace.getState();
  paintAppearance(settings);
  paintMotion(settings.reduceMotion);
  const stopSystem = subscribeSystem(() => paintMotion(useWorkspace.getState().settings.reduceMotion));
  const unsubscribe = useWorkspace.subscribe((s, prev) => {
    if (changed(s.settings, prev.settings)) paintAppearance(s.settings);
    if (s.settings.reduceMotion !== prev.settings.reduceMotion) paintMotion(s.settings.reduceMotion);
  });
  return () => {
    unsubscribe();
    stopSystem();
  };
}
