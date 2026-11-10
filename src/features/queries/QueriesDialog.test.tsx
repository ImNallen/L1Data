// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { HistoryBrowsePage, SavedQuery } from "../../ipc/bindings";
import { handleGlobalKey } from "../../shell/commands";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { QueriesDialog } from "./QueriesDialog";

const page: HistoryBrowsePage = { groups: [], counts: { all: 7, failed: 2, writes: 1 }, hasMore: false, revision: 1 };
const saved: SavedQuery = {
  id: 1,
  name: "Paid orders",
  sql: "select * from orders",
  databaseKind: "Postgres",
  excerpt: "select * from orders",
  createdAtMs: 1,
  usedAtMs: 1,
};

beforeEach(() => {
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.scrollTo = () => {};
  mockIPC((command) => (command === "history_browse" ? page : null));
  useLive.setState({ savedQueries: { rev: 1, items: [saved] } });
  window.addEventListener("keydown", handleGlobalKey);
});
afterEach(() => {
  window.removeEventListener("keydown", handleGlobalKey);
  cleanup();
  clearMocks();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

const pressed = (testId: string) => screen.getByTestId(testId).getAttribute("aria-pressed");

it("switches panes inside the same dialog, focuses the new search, and counts each tab", async () => {
  const user = userEvent.setup();
  render(<QueriesDialog />);
  act(() => useWorkspace.getState().openQueries("history"));
  const dialog = screen.getByTestId("queries-dialog");
  await waitFor(() => expect(screen.getByTestId("queries-tab-history").textContent).toBe("History7"));
  expect(screen.getByTestId("queries-tab-saved").textContent).toBe("Saved1");
  await user.click(screen.getByTestId("queries-tab-saved"));
  expect(screen.getByTestId("queries-dialog")).toBe(dialog);
  expect(screen.queryByTestId("history-list")).toBeNull();
  expect(document.activeElement).toBe(screen.getByTestId("saved-input"));
  await user.click(screen.getByTestId("queries-tab-history"));
  expect(screen.getByTestId("queries-dialog")).toBe(dialog);
  expect(screen.getByTestId("history-list")).toBeTruthy();
  expect(document.activeElement).toBe(screen.getByTestId("history-input"));
});

it("switches from History to Saved on the Saved queries shortcut, and closes from Saved", async () => {
  const user = userEvent.setup();
  render(<QueriesDialog />);
  act(() => useWorkspace.getState().openQueries("history"));
  const dialog = screen.getByTestId("queries-dialog");
  await user.keyboard("{Control>}e{/Control}");
  expect(screen.getByTestId("queries-dialog")).toBe(dialog);
  expect(pressed("queries-tab-saved")).toBe("true");
  await user.keyboard("{Control>}e{/Control}");
  await waitFor(() => expect(screen.queryByTestId("queries-dialog")).toBeNull());
});

it("locks the switch while a saved query is being edited, so the draft can't be dropped", async () => {
  const user = userEvent.setup();
  render(<QueriesDialog />);
  act(() => useWorkspace.getState().openQueries("saved"));
  await user.click(screen.getByTestId("saved-edit"));
  expect(screen.getByTestId("queries-tab-history")).toHaveProperty("disabled", true);
  await user.click(screen.getByTestId("queries-tab-history"));
  expect(screen.getByTestId("saved-name-input")).toBeTruthy();
  await user.click(screen.getByTestId("queries-close"));
  expect(screen.getByTestId("queries-dialog")).toBeTruthy();
  expect(screen.getByTestId("queries-tab-history")).toHaveProperty("disabled", false);
  await user.click(screen.getByTestId("queries-tab-history"));
  expect(screen.getByTestId("history-list")).toBeTruthy();
});
