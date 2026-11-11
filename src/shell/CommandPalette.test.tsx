// @vitest-environment jsdom
import { openWorkspaceItem } from "../testing/workspaceUI";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { docOf, dropEditor, replaceDoc, seedEditor } from "../features/editor/editorStates";
import { browseTemplate, openSqlTab } from "../features/editor/runActions";
import { useCatalog } from "../state/catalog";
import { useLive } from "../state/live";
import { activeId, allCells, ask, EMPTY_QUERY, useWorkspace } from "../state/workspace";
import { AppLayout } from "./AppLayout";
import { preloadSurfaces } from "./lazySurfaces";
import { CommandPalette } from "./CommandPalette";
import { COMMANDS, openCommandPalette, shortcutLabel } from "./commands";

// Settings and the connection dialog load from their own chunks; loaded, they open in the same pass.
beforeAll(preloadSurfaces);

beforeEach(() => {
  Element.prototype.scrollIntoView = vi.fn();
  Element.prototype.getAnimations = () => [];
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

afterEach(async () => {
  cleanup();
  await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  clearMocks();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("opens globally, preserves the search and refocuses on repeat, and cancels to its connected opener", async () => {
  const user = userEvent.setup();
  openSqlTab({ title: "draft.sql", text: "select 1" });
  render(<AppLayout />);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
  const editor = document.activeElement;
  fireEvent.keyDown(screen.getByTestId("sql-editor"), { key: "P", ctrlKey: true, shiftKey: true, isComposing: true });
  expect(screen.queryByTestId("command-palette")).toBeNull();
  await user.keyboard("{Control>}{Shift>}p{/Shift}{/Control}");
  const input = screen.getByTestId("command-input");
  expect(document.activeElement).toBe(input);
  await user.type(input, "save");
  await user.tab();
  expect(document.activeElement).toBe(screen.getByTestId("command-close"));
  await user.keyboard("{Control>}{Shift>}p{/Shift}{/Control}");
  expect(input).toHaveProperty("value", "save");
  expect(document.activeElement).toBe(input);
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(editor));
  await user.keyboard("{Control>}{Shift>}p{/Shift}{/Control}");
  expect(screen.getByTestId("command-input")).toHaveProperty("value", "");
  await user.click(screen.getByTestId("command-close"));
  await waitFor(() => expect(document.activeElement).toBe(editor));
});

it("matches every query word without case sensitivity, displays shortcuts, and handles empty results", async () => {
  const user = userEvent.setup();
  render(<CommandPalette />);
  act(openCommandPalette);
  expect(screen.queryByTestId("command-item-commands")).toBeNull();
  for (const command of COMMANDS.filter((c) => c.palette !== false && !c.available)) {
    expect(screen.getByTestId(`command-item-${command.id}`).textContent).toContain(shortcutLabel(command));
  }
  const input = screen.getByTestId("command-input");
  await user.type(input, "  CONNECTION new ");
  expect(screen.getAllByRole("option").map((row) => row.getAttribute("data-testid"))).toEqual(["command-item-new-connection"]);
  await user.clear(input);
  await user.type(input, "missing command{ArrowUp}{ArrowDown}{Enter}");
  expect(input.hasAttribute("aria-activedescendant")).toBe(false);
  expect(screen.queryAllByRole("option")).toEqual([]);
  expect(screen.getByTestId("command-empty").textContent).toBe("No commands found.");
  expect(useWorkspace.getState().overlay.kind).toBe("commands");
});

it("wraps arrow selection, guards composition keys and Escape, and runs Enter once with useful focus", async () => {
  const user = userEvent.setup();
  render(<AppLayout />);
  act(openCommandPalette);
  const input = screen.getByTestId("command-input");
  expect(screen.getByRole("option", { selected: true })).toBe(screen.getByTestId("command-item-new-query"));
  await user.keyboard("{ArrowUp}");
  expect(screen.getByRole("option", { selected: true })).toBe(screen.getByTestId("command-item-switch-notebook"));
  await user.keyboard("{ArrowDown}");
  fireEvent.compositionStart(input);
  fireEvent.keyDown(input, { key: "ArrowDown" });
  fireEvent.keyDown(input, { key: "Enter" });
  fireEvent.keyDown(input, { key: "Escape" });
  expect(screen.getByRole("option", { selected: true })).toBe(screen.getByTestId("command-item-new-query"));
  expect(allCells(useWorkspace.getState())).toHaveLength(0);
  expect(useWorkspace.getState().overlay.kind).toBe("commands");
  fireEvent.compositionEnd(input);
  fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
  expect(allCells(useWorkspace.getState())).toHaveLength(0);
  fireEvent.keyDown(input, { key: "Enter" });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(allCells(useWorkspace.getState())).toHaveLength(1);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
});

it("puts focus on the cell's title when Collapse cell hides the editor it was opened from", async () => {
  const user = userEvent.setup();
  render(<AppLayout />);
  act(() => openSqlTab({}));
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
  act(openCommandPalette);
  await user.keyboard("Collapse cell{Enter}");
  const id = activeId(useWorkspace.getState());
  expect(useWorkspace.getState().tabs[id!]?.collapsed).toBe(true);
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId(`tab-${id}`)));
});

it("filters unavailable actions reactively and rechecks a stale selection before running it", async () => {
  render(<CommandPalette />);
  act(openCommandPalette);
  for (const id of ["save", "save-as", "save-query", "close-tab"]) expect(screen.queryByTestId(`command-item-${id}`)).toBeNull();
  let tab = "";
  act(() => {
    tab = openSqlTab({ text: "select 1" });
  });
  for (const id of ["save", "save-as", "save-query", "close-tab"]) expect(screen.getByTestId(`command-item-${id}`)).toBeTruthy();
  const row = screen.getByTestId("command-item-save-query");
  const calls: string[] = [];
  mockIPC((command) => {
    calls.push(command);
    return null;
  });
  act(() => {
    useWorkspace.setState({ shown: { kind: "start", db: null } });
    fireEvent.click(row);
  });
  expect(useWorkspace.getState().overlay.kind).toBe("commands");
  expect(calls).toEqual([]);
  act(() => useWorkspace.getState().setActive(tab));
  const replacement = useWorkspace.getState().overlay;
  act(() => {
    useWorkspace.getState().open({ kind: "queries", tab: "history" });
    fireEvent.click(screen.getByTestId("command-item-new-query"));
  });
  expect(allCells(useWorkspace.getState())).toHaveLength(1);
  expect(useWorkspace.getState().overlay).not.toBe(replacement);
  expect(useWorkspace.getState().overlay).toEqual({ kind: "queries", tab: "history" });
});

it("offers Save query for edited table text and hides it again for unchanged browse text", async () => {
  const sql = "SELECT * FROM public.command_palette_orders";
  const db = { connectionId: "palette", database: "shop" };
  mockIPC((command) => (command === "table_script" ? { text: sql, acceptedTexts: [sql] } : null));
  await browseTemplate(db, "public", "command_palette_orders");
  seedEditor("browse", sql);
  useWorkspace.getState().addTab({
    id: "browse",
    title: "orders",
    console: { db, mode: "AutoCommit" },
    table: { schema: "public", table: "command_palette_orders", page: 0, query: EMPTY_QUERY },
  });
  render(<CommandPalette />);
  act(openCommandPalette);
  expect(screen.queryByTestId("command-item-save-query")).toBeNull();
  expect(screen.getByTestId("command-item-save")).toBeTruthy();
  act(() => replaceDoc("browse", "select 1"));
  expect(screen.getByTestId("command-item-save-query")).toBeTruthy();
  act(() => replaceDoc("browse", sql));
  expect(screen.queryByTestId("command-item-save-query")).toBeNull();
  expect(docOf("browse")?.toString()).toBe(sql);
});

it.each(["save", "save-as", "open-file"])("closes before %s invokes the existing native dialog action", async (id) => {
  const user = userEvent.setup();
  const calls: { command: string; overlay: string }[] = [];
  mockIPC((command) => {
    calls.push({ command, overlay: useWorkspace.getState().overlay.kind });
    return null;
  });
  openSqlTab({ text: "select 1" });
  render(<CommandPalette />);
  act(openCommandPalette);
  await user.click(screen.getByTestId(`command-item-${id}`));
  await waitFor(() => expect(calls).toContainEqual({ command: `plugin:dialog|${id === "open-file" ? "open" : "save"}`, overlay: "none" }));
  expect(screen.queryByTestId("command-palette")).toBeNull();
});

it("keeps successor dialogs focused, including an asynchronous saved-query dialog", async () => {
  const user = userEvent.setup();
  let reply: ((value: unknown) => void) | undefined;
  mockIPC((command) =>
    command === "saved_query_save"
      ? new Promise((resolve) => {
          reply = resolve;
        })
      : null,
  );
  openSqlTab({ text: "select 1" });
  render(<AppLayout />);
  act(openCommandPalette);
  await user.click(screen.getByTestId("command-item-new-connection"));
  await waitFor(() => expect(screen.getByTestId("conn-url")).toBe(document.activeElement));
  expect(screen.queryByTestId("command-palette")).toBeNull();
  act(() => useWorkspace.getState().close());
  act(openCommandPalette);
  await user.click(screen.getByTestId("command-item-save-query"));
  await waitFor(() => expect(reply).toBeTypeOf("function"));
  await act(async () => {
    reply?.({ id: 1, alreadySaved: false, queries: { rev: 2, items: [] } });
  });
  await waitFor(() => expect(document.activeElement?.closest('[role="dialog"]')?.getAttribute("data-testid")).toBe("queries-dialog"));
  await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  expect(document.activeElement?.closest('[role="dialog"]')?.getAttribute("data-testid")).toBe("queries-dialog");
});

it("preserves Settings and its filter on cancel, shows accurate labels, and can close Settings", async () => {
  const user = userEvent.setup();
  openSqlTab({ text: "select 1" });
  render(<AppLayout />);
  await openWorkspaceItem(user, "open-settings");
  const search = screen.getByTestId("settings-search");
  await user.type(search, "font");
  const settings = useWorkspace.getState().screen;
  await user.keyboard("{Control>}{Shift>}p{/Shift}{/Control}");
  expect(screen.getByTestId("command-item-close-tab").textContent).toContain("Close Settings");
  expect(screen.getByTestId("command-item-settings").textContent).toContain("Close Settings");
  for (const id of ["save", "save-as", "save-query"]) expect(screen.queryByTestId(`command-item-${id}`)).toBeNull();
  await user.keyboard("{Escape}");
  expect(useWorkspace.getState().screen).toBe(settings);
  await waitFor(() => expect(document.activeElement).toBe(search));
  expect(search).toHaveProperty("value", "font");
  await user.keyboard("{Control>}{Shift>}p{/Shift}{/Control}");
  await user.click(screen.getByTestId("command-item-close-tab"));
  expect(useWorkspace.getState().screen.kind).toBe("workspace");
  expect(allCells(useWorkspace.getState())).toHaveLength(1);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
});

it("does not restore its opener over a replacement approval dialog", async () => {
  const user = userEvent.setup();
  render(<AppLayout />);
  await user.click(screen.getByTestId("start-new-query"));
  act(openCommandPalette);
  let approval: Promise<string | null> | undefined;
  act(() => {
    approval = ask({ title: "Approve query?", body: "", choices: [{ id: "run", label: "Run", variant: "danger" }] });
  });
  await waitFor(() => expect(document.activeElement?.closest('[role="alertdialog"]')?.getAttribute("data-testid")).toBe("ask-dialog"));
  act(openCommandPalette);
  expect(screen.queryByTestId("command-palette")).toBeNull();
  await user.click(screen.getByTestId("ask-run"));
  expect(await approval).toBe("run");
});

it("groups commands without selecting headings and resets sizes without changing panel visibility", async () => {
  const user = userEvent.setup();
  useWorkspace.getState().setLayout({ sidebar: false, rightPanel: "Assistant", results: false, sidebarWidth: 470, rightPanelWidth: 540, resultsHeight: 500 });
  render(<CommandPalette />);
  act(openCommandPalette);
  expect(screen.getByText("Query").getAttribute("role")).toBe("presentation");
  expect(screen.getByText("Connections").getAttribute("role")).toBe("presentation");
  expect(screen.getByText("Workspace").getAttribute("role")).toBe("presentation");
  await user.keyboard("{ArrowDown}{ArrowDown}");
  expect(screen.getByRole("option", { selected: true })).toBe(screen.getByTestId("command-item-saved-queries"));
  await user.keyboard("{ArrowDown}");
  expect(screen.getByRole("option", { selected: true })).toBe(screen.getByTestId("command-item-new-connection"));
  await user.type(screen.getByTestId("command-input"), "reset panel");
  expect(screen.queryByText("Query")).toBeNull();
  await user.keyboard("{Enter}");
  expect(useWorkspace.getState().layout).toMatchObject({
    sidebar: false,
    rightPanel: "Assistant",
    results: false,
    sidebarWidth: 240,
    rightPanelWidth: 360,
    resultsHeight: 320,
  });
});
