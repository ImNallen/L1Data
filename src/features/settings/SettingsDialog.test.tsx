// @vitest-environment jsdom
import { openWorkspaceItem } from "../../testing/workspaceUI";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { AppLayout } from "../../shell/AppLayout";
import { preloadSurfaces } from "../../shell/lazySurfaces";
import { commandForKey } from "../../shell/commands";
import { Overlays } from "../../shell/Overlays";
import { useLive } from "../../state/live";
import { activeId, confirm, useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { openSqlTab } from "../editor/runActions";
import { dropEditor } from "../editor/editorStates";
import { QueriesDialog } from "../queries/QueriesDialog";
import { InstallingOverlay } from "../updates/InstallingOverlay";
import { SettingsDialog } from "./SettingsScreen";

// Settings and the connection dialog load from their own chunks; loaded, they open in the same pass.
beforeAll(preloadSurfaces);

beforeEach(() => {
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.getAnimations = () => [];
  Range.prototype.getClientRects = () => Object.assign([], { item: () => null });
  Range.prototype.getBoundingClientRect = () => new DOMRect();
  useWorkspace.setState((s) => ({ settings: { ...s.settings, panelMotionMs: 0 } }));
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
});

afterEach(() => {
  cleanup();
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("preserves the workspace and Settings filter while quick navigation opens above Settings", async () => {
  const user = userEvent.setup();
  useWorkspace.setState({ configs: { saved: connectionConfig("saved", "Saved connection") } });
  render(<AppLayout />);
  const main = screen.getByTestId("no-tabs");
  const tree = screen.getByTestId("conn-tree");
  await openWorkspaceItem(user, "open-settings");
  expect(screen.getByTestId("no-tabs")).toBe(main);
  expect(screen.getByTestId("conn-tree")).toBe(tree);
  expect(document.activeElement).toBe(screen.getByTestId("settings-search"));

  await user.click(screen.getByTestId("settings-section-appearance"));
  await user.type(screen.getByTestId("settings-search"), "font");
  act(() => useWorkspace.getState().setLayout({ sidebar: false }));
  await user.keyboard("{Control>}p{/Control}");
  expect(useWorkspace.getState().layout.sidebar).toBe(false);
  expect(document.activeElement).toBe(screen.getByTestId("navigation-input"));
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("settings-search")));
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "font");
  expect(useWorkspace.getState().screen).toEqual({ kind: "settings", section: "appearance", query: "font" });
});

function Dialogs() {
  return (
    <>
      <textarea data-testid="opener" />
      <SettingsDialog />
      <Overlays />
      <QueriesDialog />
    </>
  );
}

it("clears search on the first Escape, closes on the next, and returns to the shortcut opener", async () => {
  const user = userEvent.setup();
  render(<Dialogs />);
  const opener = screen.getByTestId("opener");
  opener.focus();
  act(() => useWorkspace.getState().openSettings("general"));
  await user.type(screen.getByTestId("settings-search"), "row");
  await user.keyboard("{Escape}");
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "");
  expect(screen.getByTestId("settings-breadcrumb").textContent).toBe("Settings/General");
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("settings-dialog")).toBeNull();
  await waitFor(() => expect(document.activeElement).toBe(opener));
});

it("dismisses Add theme before clearing the settings search, then dismisses the confirmation before settings", async () => {
  const user = userEvent.setup();
  render(<Dialogs />);
  act(() => useWorkspace.getState().openSettings());
  act(() => useWorkspace.getState().setSettingsQuery("theme"));
  await user.click(screen.getByTestId("add-theme"));
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("add-theme-dialog")).toBeNull();
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "theme");

  let answer: Promise<boolean> | undefined;
  act(() => {
    answer = confirm({ title: "Delete theme?", body: "Delete the custom theme.", confirmLabel: "Delete" });
  });
  expect(screen.getAllByTestId("confirm-dialog")).toHaveLength(1);
  expect(document.activeElement).toBe(screen.getByTestId("confirm-cancel"));
  await user.keyboard("{Escape}");
  await expect(answer).resolves.toBe(false);
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "theme");
  await user.keyboard("{Escape}");
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "");
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("settings-dialog")).toBeNull();
});

it("hands focus to a new dialog when a command leaves settings", async () => {
  const user = userEvent.setup();
  render(<Dialogs />);
  const opener = screen.getByTestId("opener");
  opener.focus();
  act(() => useWorkspace.getState().openSettings());
  act(() => {
    commandForKey({ key: "e", ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }, false)?.run();
  });
  await user.keyboard("orders");
  expect(document.activeElement).toBe(screen.getByTestId("saved-input"));
  expect(screen.getByTestId("saved-input")).toHaveProperty("value", "orders");
});

it("closes theme creation before clearing settings search or closing settings", async () => {
  const user = userEvent.setup();
  render(<Dialogs />);
  act(() => useWorkspace.getState().openSettings());
  await user.type(screen.getByTestId("settings-search"), "theme");
  await user.click(screen.getByTestId("create-theme"));
  expect(screen.getByTestId("create-theme-panel").contains(document.activeElement)).toBe(true);
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("create-theme-panel")).toBeNull();
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "theme");
  await user.keyboard("{Escape}");
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "");
  expect(screen.getByTestId("settings-dialog")).toBeTruthy();
});

it("cancels a field draft before clearing settings search", async () => {
  const user = userEvent.setup();
  render(<Dialogs />);
  act(() => useWorkspace.getState().openSettings("general"));
  await user.type(screen.getByTestId("settings-search"), "row");
  const input = screen.getByTestId("setting-maxRows");
  await user.clear(input);
  await user.type(input, "123");
  await user.keyboard("{Escape}");
  expect(screen.getByTestId("settings-search")).toHaveProperty("value", "row");
  expect(screen.getByTestId("setting-maxRows")).toHaveProperty("value", "200000");
  expect(useWorkspace.getState().settings.maxRows).toBe(200000);
});

it("hands focus to installation and restores settings after installation ends", async () => {
  const user = userEvent.setup();
  render(
    <>
      <Dialogs />
      <InstallingOverlay />
    </>,
  );
  act(() => useWorkspace.getState().openSettings("general"));
  const search = screen.getByTestId("settings-search");
  act(() =>
    useLive.setState({
      updates: { rev: 1, track: "Stable", currentVersion: "0.1.0", checkedAtMs: null, status: { kind: "Installing", version: "0.2.0" } },
    }),
  );
  const blocker = screen.getByTestId("update-installing");
  expect(document.activeElement).toBe(blocker);
  search.focus();
  expect(document.activeElement).toBe(blocker);
  await user.keyboard("{Escape}text");
  expect(screen.getByTestId("settings-dialog")).toBeTruthy();
  expect(search).toHaveProperty("value", "");
  act(() =>
    useLive.setState({
      updates: { rev: 2, track: "Stable", currentVersion: "0.1.0", checkedAtMs: null, status: { kind: "Failed", failure: { kind: "ReadOnlyLocation" } } },
    }),
  );
  expect(screen.queryByTestId("update-installing")).toBeNull();
  await waitFor(() => expect(document.activeElement).toBe(search));
});

it("keeps focus in the new editor when the new-tab shortcut leaves settings", async () => {
  const user = userEvent.setup();
  render(<AppLayout />);
  await openWorkspaceItem(user, "open-settings");
  await user.keyboard("{Control>}t{/Control}");
  const editor = screen.getByTestId("sql-editor");
  expect(editor.contains(document.activeElement)).toBe(true);
  const input = document.activeElement;
  await user.keyboard("{Control>},{/Control}");
  expect(screen.getByTestId("settings-dialog")).toBeTruthy();
  expect(screen.getByTestId("sql-editor")).toBe(editor);
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(input));
});

it("keeps cell actions keyboard accessible when another editor activates", async () => {
  const user = userEvent.setup();
  let first = "",
    second = "";
  act(() => {
    first = openSqlTab();
    second = openSqlTab();
  });
  render(<AppLayout />);
  await user.click(screen.getByTestId(`tab-${first}`));
  expect(activeId(useWorkspace.getState())).toBe(first);
  expect(document.activeElement).toBe(screen.getByTestId(`tab-${first}`));
  await user.tab();
  expect(document.activeElement?.getAttribute("aria-label")).toBe("Format SQL");
  await user.tab();
  expect(document.activeElement).toBe(screen.getByTestId("run-btn"));
  await user.click(screen.getByTestId(`tab-${second}`));
  expect(activeId(useWorkspace.getState())).toBe(second);
  expect(document.activeElement).toBe(screen.getByTestId(`tab-${second}`));
});

it("returns the global close-tab shortcut from the editor to the start action after the last close", async () => {
  const user = userEvent.setup();
  act(() => {
    openSqlTab();
  });
  render(<AppLayout />);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
  await user.keyboard("{Control>}w{/Control}");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("start-new-query")));
});

it("opens General first and persists assistant access through its searchable control", async () => {
  const { mockIPC, clearMocks } = await import("@tauri-apps/api/mocks");
  const writes: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "settings_set") writes.push(args);
  });
  const user = userEvent.setup();
  render(<Dialogs />);
  act(() => useWorkspace.getState().openSettings());
  expect(screen.getByTestId("settings-breadcrumb").textContent).toBe("Settings/General");
  expect(screen.getByTestId("setting-assistantAccess")).toHaveProperty("value", "SchemaOnly");
  expect(screen.getByText(/Everything you type or paste into chat/)).toBeTruthy();
  await user.selectOptions(screen.getByTestId("setting-assistantAccess"), "Full");
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toMatchObject({ settings: { assistantAccess: "Full" } });
  await user.type(screen.getByTestId("settings-search"), "privacy");
  expect(screen.getByTestId("setting-assistantAccess")).toHaveProperty("value", "Full");
  await user.selectOptions(screen.getByTestId("setting-assistantAccess"), "SchemaOnly");
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[1]).toMatchObject({ settings: { assistantAccess: "SchemaOnly" } });
  clearMocks();
});
