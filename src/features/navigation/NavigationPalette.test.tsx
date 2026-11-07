// @vitest-environment jsdom
import { openWorkspaceItem } from "../../testing/workspaceUI";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { AppLayout } from "../../shell/AppLayout";
import { preloadSurfaces } from "../../shell/lazySurfaces";
import { useCatalog } from "../../state/catalog";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { dropEditor, textOf } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { openNavigation } from "./navigationActions";
import { NavigationPalette } from "./NavigationPalette";

// Settings and the connection dialog load from their own chunks; loaded, they open in the same pass.
beforeAll(preloadSurfaces);

beforeEach(() => {
  Element.prototype.scrollIntoView = vi.fn();
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
  for (const id of useWorkspace.getState().order) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("opens globally from the editor with a hidden sidebar, retains its query on repeat, and cancels back to the editor", async () => {
  const user = userEvent.setup();
  openSqlTab({ title: "draft.sql", text: "select 1" });
  const current = openSqlTab({ title: "other.sql" });
  useWorkspace.getState().setLayout({ sidebar: false });
  useWorkspace.getState().setTreeQuery("orders");
  render(<AppLayout />);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
  const editor = document.activeElement;
  fireEvent.keyDown(screen.getByTestId("sql-editor"), { key: "p", ctrlKey: true, isComposing: true });
  expect(screen.queryByTestId("navigation-palette")).toBeNull();
  await user.keyboard("{Control>}p{/Control}");
  const input = screen.getByRole("combobox", { name: "Quick navigation" });
  expect(document.activeElement).toBe(input);
  await user.type(input, "draft");
  await user.keyboard("{Control>}p{/Control}");
  expect(input).toHaveProperty("value", "draft");
  expect(document.activeElement).toBe(input);
  expect(screen.getByTestId("navigation-scope").textContent).toContain("loaded catalogs only");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(editor));
  expect(useWorkspace.getState().layout.sidebar).toBe(false);
  expect(useWorkspace.getState().treeQuery).toBe("orders");
  await user.keyboard("{Control>}p{/Control}");
  await user.tab();
  expect(document.activeElement).toBe(screen.getByTestId("navigation-close"));
  await user.keyboard("{Enter}");
  await waitFor(() => expect(document.activeElement).toBe(editor));
  expect(useWorkspace.getState().active).toBe(current);
});

it("navigates with arrows and Enter, ignores IME Enter, and focuses the selected tab's editor", async () => {
  const user = userEvent.setup();
  const first = openSqlTab({ title: "first.sql", text: "select 'first'" });
  const second = openSqlTab({ title: "second.sql", text: "select 'second'" });
  render(<AppLayout />);
  await user.keyboard("{Control>}p{/Control}");
  const input = screen.getByTestId("navigation-input");
  expect(screen.getAllByRole("option").map((row) => row.getAttribute("aria-selected"))).toEqual(["true", "false"]);
  fireEvent.compositionStart(input);
  fireEvent.keyDown(input, { key: "Enter", isComposing: true });
  expect(useWorkspace.getState().active).toBe(second);
  expect(screen.getByTestId("navigation-palette")).toBeTruthy();
  fireEvent.compositionEnd(input);
  await user.keyboard("{ArrowUp}");
  expect(input.getAttribute("aria-activedescendant")).toBe(screen.getAllByRole("option")[1]?.id);
  await user.keyboard("{ArrowDown}{Enter}");
  expect(useWorkspace.getState().active).toBe(first);
  await waitFor(() => expect(screen.queryByTestId("navigation-palette")).toBeNull());
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
  expect(textOf(first)).toBe("select 'first'");
  expect(textOf(second)).toBe("select 'second'");
});

it("preserves selected identity when matches change and falls back to a present option when it disappears", async () => {
  const user = userEvent.setup();
  const first = openSqlTab({ title: "first.sql" });
  const second = openSqlTab({ title: "second.sql" });
  render(<NavigationPalette />);
  act(openNavigation);
  const input = screen.getByTestId("navigation-input");
  await user.keyboard("{ArrowDown}");
  await user.type(input, "sql");
  expect(screen.getByRole("option", { selected: true }).textContent).toContain("second.sql");
  act(() => useWorkspace.getState().closeTab(second));
  dropEditor(second);
  expect(screen.getByRole("option", { selected: true }).textContent).toContain("first.sql");
  expect(input.getAttribute("aria-activedescendant")).toBe(screen.getByRole("option").id);
  await user.keyboard("{Enter}");
  expect(useWorkspace.getState().active).toBe(first);
});

it("opens the same navigation from Settings and activates a database without changing the previous query target", async () => {
  const user = userEvent.setup();
  const db = { connectionId: "c", database: "shop" };
  useWorkspace.getState().upsertConfig(connectionConfig("c", "Local"));
  useLive.setState({ connections: { c: connectedState("c", [databaseState("shop", true, []), databaseState("reporting", false, [])]) } });
  const previous = openSqlTab({ db, title: "work.sql", text: "begin;" });
  const original = useWorkspace.getState().tabs[previous];
  render(<AppLayout />);
  await openWorkspaceItem(user, "open-settings");
  await user.keyboard("{Control>}p{/Control}");
  await user.type(screen.getByTestId("navigation-input"), "reporting");
  expect(screen.getByRole("option").textContent).toContain("Open new query in this database");
  await user.click(screen.getByRole("option"));
  expect(useWorkspace.getState().screen.kind).toBe("workspace");
  const { active, tabs } = useWorkspace.getState();
  expect(active && tabs[active]?.console.db).toEqual({ connectionId: "c", database: "reporting" });
  expect(tabs[previous]).toBe(original);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
});

it("leaves an empty search with no active descendant and ignores Enter", async () => {
  const user = userEvent.setup();
  render(<NavigationPalette />);
  act(openNavigation);
  const input = screen.getByTestId("navigation-input");
  await user.type(input, "missing{ArrowDown}{ArrowUp}{Enter}");
  expect(input.hasAttribute("aria-activedescendant")).toBe(false);
  expect(screen.queryAllByRole("option")).toEqual([]);
  expect(screen.getByTestId("navigation-empty").textContent).toContain("No destinations");
  expect(useWorkspace.getState().overlay.kind).toBe("navigation");
});
