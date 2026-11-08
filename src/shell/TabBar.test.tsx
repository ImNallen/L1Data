// @vitest-environment jsdom
import { mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { dropEditor, insertAtCursor, seedEditor } from "../features/editor/editorStates";
import { useLive } from "../state/live";
import { activeId, allCells, useWorkspace, type Tab } from "../state/workspace";
import { handleGlobalKey } from "./commands";
import { Overlays } from "./Overlays";
import { TabBar } from "./TabBar";
import { cellsState } from "../testing/notebooks";

const tab = (id: string): Tab => ({ id, title: `${id}.sql`, console: { db: null, mode: "AutoCommit" }, table: null, file: null });
const ws = () => useWorkspace.getState();

function Workspace() {
  const empty = useWorkspace((s) => allCells(s).length === 0);
  return (
    <>
      <TabBar />
      {empty && <button data-testid="start-new-query">New query tab</button>}
      <Overlays />
    </>
  );
}

beforeEach(() => {
  window.addEventListener("keydown", handleGlobalKey);
  Element.prototype.scrollIntoView = vi.fn();
  useWorkspace.setState(cellsState([tab("a"), tab("b"), tab("c")], "b"));
  mockIPC(() => null);
});

afterEach(() => {
  window.removeEventListener("keydown", handleGlobalKey);
  cleanup();
  for (const id of ["a", "b", "c"]) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.restoreAllMocks();
});

it("moves focus and selection through the existing order, wrapping and using Home and End", async () => {
  const user = userEvent.setup();
  render(<Workspace />);
  const selected = screen.getByTestId("tab-b");
  selected.focus();
  expect(screen.getAllByRole("tab").filter((el) => el.tabIndex === 0)).toEqual([selected]);
  const moves: [string, string][] = [
    ["{ArrowRight}", "c"],
    ["{ArrowRight}", "a"],
    ["{ArrowLeft}", "c"],
    ["{Home}", "a"],
    ["{End}", "c"],
  ];
  for (const [key, id] of moves) {
    await user.keyboard(key);
    await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId(`tab-${id}`)));
    expect(activeId(ws())).toBe(id);
    expect(screen.getByTestId(`tab-${id}`).getAttribute("aria-selected")).toBe("true");
  }
  expect(screen.getByTestId("tab-c").scrollIntoView).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });
});

it("activates a focused tab with Enter or Space and keeps close controls outside the tab", async () => {
  const user = userEvent.setup();
  render(<Workspace />);
  screen.getByTestId("tab-a").focus();
  await user.keyboard("{Enter}");
  expect(activeId(ws())).toBe("a");
  screen.getByTestId("tab-c").focus();
  await user.keyboard(" ");
  expect(activeId(ws())).toBe("c");
  expect(screen.getByRole("button", { name: "Close c.sql" }).closest('[role="tab"]')).toBeNull();
});

it("ignores navigation keys from close and rename controls and returns focus after rename", async () => {
  const user = userEvent.setup();
  render(<Workspace />);
  screen.getByTestId("tab-close-b").focus();
  await user.keyboard("{ArrowRight}{Home}{End}");
  expect(activeId(ws())).toBe("b");
  await user.dblClick(screen.getByTestId("tab-b"));
  const input = screen.getByRole("textbox", { name: "Tab name" });
  await user.clear(input);
  await user.type(input, "Revenue by region");
  await user.keyboard("{Home}{ArrowRight}{End}");
  expect(activeId(ws())).toBe("b");
  await user.keyboard("{Enter}");
  expect(ws().tabs.b?.title).toBe("Revenue by region");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("tab-b")));
  await user.dblClick(screen.getByTestId("tab-b"));
  await user.clear(screen.getByRole("textbox", { name: "Tab name" }));
  await user.type(screen.getByRole("textbox", { name: "Tab name" }), "Discard rename");
  await user.keyboard("{Escape}");
  expect(ws().tabs.b?.title).toBe("Revenue by region");
});

it("returns keyboard focus to the selected neighbor, then to the first start action after the last close", async () => {
  const user = userEvent.setup();
  render(<Workspace />);
  const closes: [string, string | null][] = [
    ["b", "c"],
    ["c", "a"],
    ["a", null],
  ];
  for (const [id, next] of closes) {
    screen.getByTestId(`tab-close-${id}`).focus();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId(next ? `tab-${next}` : "start-new-query")));
    expect(ws().tabs[id]).toBeUndefined();
  }
  fireEvent.keyDown(screen.getByRole("tablist"), { key: "ArrowRight" });
  expect(allCells(ws())).toEqual([]);
});

it("keeps middle-click close and the existing keyboard close command", async () => {
  const user = userEvent.setup();
  render(<Workspace />);
  fireEvent.mouseDown(screen.getByTestId("tab-c"), { button: 1 });
  await waitFor(() => expect(screen.queryByTestId("tab-c")).toBeNull());
  screen.getByTestId("tab-b").focus();
  await user.keyboard("{Control>}w{/Control}");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("tab-a")));
});

it("keeps a dirty file and useful focus when its save question is cancelled", async () => {
  const user = userEvent.setup();
  seedEditor("b", "select 1;", "seeded");
  ws().bindFile("b", { binding: { path: "/tmp/b.sql", stamp: "s1", format: { bom: false, crlf: false } }, name: "b.sql" });
  insertAtCursor("b", "-- edited ");
  render(<Workspace />);
  screen.getByTestId("tab-close-b").focus();
  await user.keyboard("{Enter}");
  await screen.findByTestId("ask-dialog");
  await user.click(screen.getByTestId("ask-cancel"));
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("tab-close-b")));
  expect(allCells(ws())).toEqual(["a", "b", "c"]);
  expect(screen.getByTestId("tab-unsaved-b")).toBeTruthy();
});

it("keeps transaction status and the tab after cancelling rollback, then restores focus after confirmation", async () => {
  const user = userEvent.setup();
  act(() =>
    useLive.setState({
      consoles: {
        b: {
          tabId: "b",
          rev: 1,
          config: { connectionId: "c1", database: "shop", schema: null, mode: "Manual" },
          session: { kind: "Open", backendPid: 7 },
          txn: "Aborted",
          txnDepth: null,
          work: null,
          export: null,
          import: null,
          run: { kind: "Idle" },
        },
      },
    }),
  );
  mockIPC((cmd, args) => {
    if (cmd === "console_close" && args && "force" in args && !args.force) throw { kind: "OpenTransactions", tabs: [] };
    return null;
  });
  render(<Workspace />);
  expect(screen.getByRole("img", { name: "Transaction aborted" })).toBeTruthy();
  screen.getByTestId("tab-close-b").focus();
  await user.keyboard("{Enter}");
  await screen.findByTestId("confirm-dialog");
  await user.click(screen.getByTestId("confirm-cancel"));
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("tab-close-b")));
  expect(ws().tabs.b).toBeTruthy();
  await user.keyboard("{Enter}");
  await user.click(await screen.findByTestId("confirm-btn"));
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("tab-c")));
});
