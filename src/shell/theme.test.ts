import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ColorScheme, Settings } from "../ipc/bindings";
import { useWorkspace } from "../state/workspace";
import { BUILT_IN_FAMILIES } from "../themes/catalog";
import { fakeRoot, type FakeRoot } from "../testing/fakeRoot";
import { previewTheme, startTheme } from "./theme";

let root: FakeRoot;
let system: { matches: boolean; listeners: (() => void)[] };

beforeEach(() => {
  root = fakeRoot();
  system = { matches: true, listeners: [] };
  vi.stubGlobal("document", { documentElement: root });
  vi.stubGlobal("window", {
    matchMedia: () => ({
      get matches() {
        return system.matches;
      },
      addEventListener: (_: string, fn: () => void) => system.listeners.push(fn),
      removeEventListener: (_: string, fn: () => void) => system.listeners.splice(system.listeners.indexOf(fn), 1),
    }),
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

const setSettings = (patch: Partial<Settings>) => useWorkspace.setState({ settings: { ...useWorkspace.getState().settings, ...patch } });
const setScheme = (colorScheme: ColorScheme) => setSettings({ colorScheme });
const setSystemDark = (dark: boolean) => {
  system.matches = dark;
  for (const fn of system.listeners) fn();
};

it("paints the document with the chosen theme and follows the system only while the choice is System", () => {
  const stop = startTheme();
  expect([root.dataset.theme, root.style.colorScheme]).toEqual(["dark", "dark"]);

  setSystemDark(false);
  expect([root.dataset.theme, root.style.colorScheme]).toEqual(["light", "light"]);

  setScheme("Dark");
  expect(root.dataset.theme).toBe("dark");
  setSystemDark(true);
  setSystemDark(false);
  expect(root.dataset.theme).toBe("dark");

  setScheme("Light");
  setSystemDark(true);
  expect(root.dataset.theme).toBe("light");

  setScheme("System");
  expect(root.dataset.theme).toBe("dark");

  stop();
  setScheme("Light");
  expect(root.dataset.theme).toBe("dark");
});

const bg = () => root.style.vars.get("--color-bg");

it("paints a theme's palette inline and clears it when the pick returns to L1", () => {
  const stop = startTheme();
  expect(root.style.vars.size).toBe(0);

  setSettings({ darkTheme: "grove-dark" });
  expect([root.dataset.theme, bg(), root.style.vars.get("--color-run")]).toEqual(["dark", "#0c1310", "#2f9e6e"]);
  expect(root.style.vars.size).toBe(49);

  setSettings({ darkTheme: "l1-dark" });
  expect(root.dataset.theme).toBe("dark");
  expect(root.style.vars.size).toBe(0);
  stop();
});

it("paints the light pick or the dark pick as the scheme resolves", () => {
  setSettings({ lightTheme: "ocean-light", darkTheme: "rose-dark" });
  const stop = startTheme();
  expect([root.dataset.theme, bg()]).toEqual(["dark", "#150d11"]);

  setSystemDark(false);
  expect([root.dataset.theme, bg()]).toEqual(["light", "#f6f9fc"]);

  setScheme("Dark");
  expect([root.dataset.theme, bg()]).toEqual(["dark", "#150d11"]);
  stop();
});

it("paints a saved custom theme with its stored colours over the derived ones, ignoring unknown or invalid entries", () => {
  setSettings({
    darkTheme: "custom-1-0",
    customThemes: [
      {
        id: "custom-1",
        name: "Dusk",
        variants: [
          {
            id: "custom-1-0",
            name: "Dusk",
            appearance: "dark",
            colors: { background: "#202020", accent: "#ff0000", keyword: "#123456", results: "#0d0d0d", bogus: "#ffffff", muted: "red" },
          },
        ],
      },
    ],
  });
  const stop = startTheme();
  stop();
  expect([bg(), root.style.vars.get("--color-run"), root.style.vars.get("--color-keyword")]).toEqual(["#202020", "#ff0000", "#123456"]);
  expect(root.style.vars.get("--color-muted")).toMatch(/^#[0-9a-f]{6}$/);
  expect([...root.style.vars.keys()].every((k) => k.startsWith("--"))).toBe(true);
  expect(root.style.vars.size).toBe(49);
});

it("paints a previewed theme over the saved one and restores the saved one when the preview ends", () => {
  const stop = startTheme();
  const emberLight = BUILT_IN_FAMILIES.flatMap((f) => f.variants).find((v) => v.id === "ember-light") ?? null;
  previewTheme(emberLight);
  expect([root.dataset.theme, bg()]).toEqual(["light", "#fdf9f6"]);

  setSettings({ darkTheme: "iris-dark" });
  expect(bg()).toBe("#fdf9f6");

  previewTheme(null);
  expect([root.dataset.theme, bg()]).toEqual(["dark", "#0f0e17"]);
  stop();
});

// The webview reports the window's forced theme until set_theme lands, then the OS appearance, which is dark here.
function inTauriWindow(delays: Record<string, number> = {}) {
  vi.stubGlobal("isTauri", true);
  vi.stubGlobal("getComputedStyle", () => ({ getPropertyValue: () => "#000" }));
  mockWindows("main");
  mockIPC((cmd, args) => {
    if (cmd !== "plugin:window|set_theme") return null;
    const forced = (args as { value: "light" | "dark" | null }).value;
    return new Promise((resolve) =>
      setTimeout(
        () => {
          const dark = forced === null ? true : forced === "dark";
          if (dark !== system.matches) setSystemDark(dark);
          resolve(null);
        },
        delays[String(forced)] ?? 5,
      ),
    );
  });
  const painted: string[] = [];
  root.dataset = new Proxy(root.dataset, {
    set(target, key, value) {
      if (key === "theme") painted.push(value);
      target[key as string] = value;
      return true;
    },
  });
  return painted;
}

const settle = () => new Promise((r) => setTimeout(r, 50));

it("in the app window, paints System from the OS only after the window drops its forced theme", async () => {
  const painted = inTauriWindow();
  setScheme("Light");
  system.matches = false;
  const stop = startTheme();
  await settle();
  painted.length = 0;

  setScheme("System");
  await settle();
  stop();
  expect(painted).not.toContain("light");
  expect(painted.at(-1)).toBe("dark");
});

it("in the app window, never paints an earlier choice whose window theme change lands last", async () => {
  const painted = inTauriWindow({ light: 30, dark: 5 });
  const stop = startTheme();
  await settle();
  painted.length = 0;

  setScheme("Light");
  setScheme("Dark");
  await settle();
  stop();
  expect(painted).not.toContain("light");
  expect(painted.at(-1)).toBe("dark");
});
