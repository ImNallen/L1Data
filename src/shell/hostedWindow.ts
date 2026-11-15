import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

export type WindowControl = "minimize" | "maximize" | "close";
export type ResizeEdge = "North" | "South" | "East" | "West" | "NorthEast" | "NorthWest" | "SouthEast" | "SouthWest";

function isMac(userAgent: string): boolean {
  return /Macintosh|Mac OS X/.test(userAgent);
}

export const MAC = typeof navigator !== "undefined" && isMac(navigator.userAgent);
export const MOD_LABEL = MAC ? "⌘" : "Ctrl+";

export type CustomChrome = {
  chrome: "custom";
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  maximized(): Promise<boolean>;
  onResize(fn: () => void): Promise<() => void>;
  startResize(edge: ResizeEdge): Promise<void>;
};

export type HostedWindow = { chrome: "native" } | CustomChrome;

export function hostedWindow(userAgent?: string): HostedWindow | null {
  if (!isTauri()) return null;
  if (isMac(userAgent ?? navigator.userAgent)) return { chrome: "native" };
  const win = getCurrentWindow();
  return {
    chrome: "custom",
    minimize: () => win.minimize(),
    toggleMaximize: () => win.toggleMaximize(),
    close: () => win.close(),
    maximized: () => win.isMaximized(),
    onResize: (fn) => win.onResized(() => fn()),
    startResize: (edge) => win.startResizeDragging(edge),
  };
}
