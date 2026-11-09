// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState, DatabaseRef } from "../../ipc/bindings";
import { AppLayout } from "../../shell/AppLayout";
import { preloadSurfaces } from "../../shell/lazySurfaces";
import { useCatalog } from "../../state/catalog";
import { useLive } from "../../state/live";
import { activeId, shownDb, shownNotebook, useWorkspace } from "../../state/workspace";
import { displayPath } from "../../state/notebookFile";
import { connectionConfig } from "../../testing/connections";
import { bindToFile } from "../../testing/notebooks";
import { dropEditor, insertAtCursor } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";

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
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const ws = () => useWorkspace.getState();
const shop: DatabaseRef = { connectionId: "c", database: "shop" };
const postgres: DatabaseRef = { connectionId: "c", database: "postgres" };

function consoleState(tabId: string, txn: ConsoleState["txn"]): ConsoleState {
  return {
    tabId,
    rev: 1,
    config: { connectionId: "c", database: "postgres", schema: null, mode: "Manual" },
    session: { kind: "Open", backendPid: 7 },
    txn,
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
}

/** shop holds "Revenue" (2 cells) and "Refunds" (shown), postgres holds "Nightly" with an open transaction. */
function threeNotebooks() {
  ws().upsertConfig(connectionConfig("c", "Local"));
  const nightly = openSqlTab({ db: postgres, text: "begin" });
  useLive.setState({ consoles: { [nightly]: consoleState(nightly, "Open") } });
  const revenue = openSqlTab({ db: shop, text: "select 1" });
  openSqlTab({ db: shop, text: "select 2" });
  const refunds = openSqlTab({ db: shop, text: "select 3", newNotebook: true });
  const id = (cell: string) => Object.values(ws().notebooks).find((n) => n.cells.includes(cell))!.id;
  ws().renameNotebook(id(revenue), "Revenue");
  ws().renameNotebook(id(refunds), "Refunds");
  ws().renameNotebook(id(nightly), "Nightly");
  return { revenue: id(revenue), refunds: id(refunds), nightly: id(nightly) };
}

const rows = () =>
  screen.getAllByRole("option").map((row) => {
    const text = within(row);
    return [row.getAttribute("aria-label"), row.getAttribute("aria-selected") === "true", text.queryByText(/Switch|Open now/)?.textContent ?? null];
  });

it("opens from the Notebooks button, groups the notebooks, starts on the first one not open, and switches with the keyboard", async () => {
  const user = userEvent.setup();
  const { revenue } = threeNotebooks();
  render(<AppLayout />);
  const button = screen.getByRole("button", { name: "Notebooks" });
  expect([button.title, button.getAttribute("aria-pressed")]).toEqual(["Refunds", "false"]);
  await user.click(button);
  expect(button.getAttribute("aria-pressed")).toBe("true");
  const input = screen.getByRole("combobox", { name: "Search notebooks" });
  expect(document.activeElement).toBe(input);
  expect(rows()).toEqual([
    ["Refunds", false, "Open now"],
    ["Revenue", true, "Switch"],
    ["Nightly", false, "Switch database"],
    ["New notebook", false, null],
  ]);
  expect(screen.getByTestId("notebook-switcher-count").textContent).toBe("3 notebooks");
  expect(screen.getByRole("option", { name: "Nightly" }).textContent).toContain("postgres · Local · Open transaction");
  expect(screen.getByRole("option", { name: "Revenue" }).textContent).toContain("2 cells");
  expect(screen.getByRole("option", { name: "New notebook" }).textContent).toContain("Start an empty notebook on shop");
  await user.type(input, "re");
  expect(rows().map(([title]) => title)).toEqual(["Refunds", "Revenue", "New notebook"]);
  expect(screen.getByTestId("notebook-switcher-count").textContent).toBe("2 of 3 notebooks");
  await user.keyboard("{Enter}");
  await waitFor(() => expect(screen.queryByTestId("notebook-switcher")).toBeNull());
  expect(shownNotebook(ws())?.id).toBe(revenue);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
  ws().showDatabase(postgres);
  ws().showDatabase(shop);
  expect(shownNotebook(ws())?.id).toBe(revenue);
});

it("shows each notebook's file, with the home folder as ~, finds a notebook by its path, and marks the shown one unsaved", async () => {
  const user = userEvent.setup();
  const { revenue, refunds } = threeNotebooks();
  useLive.setState({ homeDir: "/Users/a" });
  bindToFile(revenue, "/Users/a/reports/revenue.sql");
  bindToFile(refunds, "/srv/shared/refunds.sql");
  render(<AppLayout />);
  expect(screen.getByRole("button", { name: "Notebooks" }).title).toBe("Refunds");
  act(() => insertAtCursor(ws().notebooks[refunds]!.cells[0]!, "-- edited "));
  const button = screen.getByRole("button", { name: "Notebooks, unsaved changes" });
  expect(button.title).toBe("Refunds has unsaved changes");
  await user.click(button);
  const context = (name: string) => screen.getByRole("option", { name }).querySelector("[title]")?.textContent;
  expect([context("Refunds"), context("Revenue"), context("Nightly")]).toEqual([
    "/srv/shared/refunds.sql",
    "~/reports/revenue.sql",
    "Not saved to a file · postgres · Local · Open transaction",
  ]);
  await user.type(screen.getByRole("combobox", { name: "Search notebooks" }), "reports/");
  expect(rows().map(([title]) => title)).toEqual(["Revenue", "New notebook"]);
});

it("shows a path under the home folder from ~", () => {
  expect(displayPath("/Users/a/q.sql", "/Users/a")).toBe("~/q.sql");
  expect(displayPath("/Users/ab/q.sql", "/Users/a")).toBe("/Users/ab/q.sql");
  expect(displayPath("C:\\Users\\a\\q.sql", "C:\\Users\\a")).toBe("~\\q.sql");
  expect(displayPath("/Users/a/q.sql", null)).toBe("/Users/a/q.sql");
});

it("switches the database with a notebook from another one, and opens from the command panel", async () => {
  const user = userEvent.setup();
  const { nightly } = threeNotebooks();
  render(<AppLayout />);
  await user.keyboard("{Control>}{Shift>}p{/Shift}{/Control}");
  await user.type(screen.getByTestId("command-input"), "switch notebook{Enter}");
  await user.click(await screen.findByRole("option", { name: "Nightly" }));
  expect([shownNotebook(ws())?.id, shownDb(ws())]).toEqual([nightly, postgres]);
});

it("starts a new notebook named after its database, with one empty cell", async () => {
  const user = userEvent.setup();
  threeNotebooks();
  render(<AppLayout />);
  await user.click(screen.getByRole("button", { name: "Notebooks" }));
  await user.click(screen.getByRole("option", { name: "New notebook" }));
  const notebook = shownNotebook(ws());
  expect([notebook?.title, notebook?.db, notebook?.cells.length]).toEqual(["shop", shop, 1]);
  await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("SQL editor"));
});

it("renames the selected notebook in place with F2, keeps the old title for a blank one, and cancels with Esc", async () => {
  const user = userEvent.setup();
  const { revenue } = threeNotebooks();
  render(<AppLayout />);
  await user.click(screen.getByRole("button", { name: "Notebooks" }));
  await user.keyboard("{F2}");
  const rename = screen.getByRole("textbox", { name: "Rename Revenue" });
  expect(document.activeElement).toBe(rename);
  await user.clear(rename);
  await user.type(rename, "Revenue by region{Enter}");
  expect(ws().notebooks[revenue]?.title).toBe("Revenue by region");
  expect(document.activeElement).toBe(screen.getByRole("combobox", { name: "Search notebooks" }));
  await user.keyboard("{F2}");
  await user.clear(screen.getByRole("textbox", { name: "Rename Revenue by region" }));
  await user.keyboard("{Enter}");
  expect(ws().notebooks[revenue]?.title).toBe("Revenue by region");
  await user.keyboard("{F2}x{Escape}");
  expect([ws().notebooks[revenue]?.title, ws().overlay.kind]).toEqual(["Revenue by region", "notebooks"]);
  expect(screen.queryByRole("textbox", { name: /Rename/ })).toBeNull();
  await user.click(screen.getByTestId(`notebook-actions-${revenue}`));
  await user.click(await screen.findByTestId("notebook-rename"));
  await user.keyboard("{Control>}a{/Control}Q3{Enter}");
  expect(ws().notebooks[revenue]?.title).toBe("Q3");
});

it("deletes the selected notebook with Ctrl+Backspace after asking", async () => {
  const user = userEvent.setup();
  const { revenue } = threeNotebooks();
  render(<AppLayout />);
  await user.click(screen.getByRole("button", { name: "Notebooks" }));
  await user.keyboard("{Control>}{Backspace}{/Control}");
  const dialog = await screen.findByTestId("confirm-dialog");
  expect(dialog.textContent).toContain("Delete Revenue?");
  expect(dialog.textContent).toContain("Its 2 cells close.");
  expect(document.activeElement).toBe(screen.getByTestId("confirm-cancel"));
  await user.click(screen.getByTestId("confirm-btn"));
  await waitFor(() => expect(ws().notebooks[revenue]).toBeUndefined());
  expect(Object.values(ws().notebooks).map((n) => n.title)).toEqual(["Nightly", "Refunds"]);
  expect(activeId(ws())).not.toBeNull();
});

it("lists the notebook start commands on a database with no notebook", () => {
  act(() => ws().showDatabase(shop));
  render(<AppLayout />);
  const start = screen.getByTestId("no-tabs");
  expect(within(start).queryByRole("heading")).toBeNull();
  const lists = within(start).getAllByRole("list");
  expect(
    lists.map((list) =>
      within(list)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ),
  ).toEqual([
    ["New notebookCtrl+T", "Open notebook file…Ctrl+O", "Switch notebook…"],
    ["Quick navigationCtrl+P", "Saved queriesCtrl+E"],
  ]);
});
