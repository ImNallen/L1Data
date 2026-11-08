// @vitest-environment jsdom
import { mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { ConsoleState, DatabaseRef } from "../../ipc/bindings";
import { Overlays } from "../../shell/Overlays";
import { useLive } from "../../state/live";
import { allCells, useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { dropEditor } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { confirmDelete } from "./connectionActions";

const ws = () => useWorkspace.getState();
const db = (connectionId: string, database: string): DatabaseRef => ({ connectionId, database });

let calls: string[];

beforeEach(() => {
  calls = [];
  mockIPC((cmd, args) => {
    if (cmd === "console_close") calls.push(`close ${(args as { tabId: string; force: boolean }).force}`);
    if (cmd === "connection_delete") calls.push("delete");
    return null;
  });
});

afterEach(() => {
  cleanup();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

function openTransaction(tabId: string): ConsoleState {
  return {
    tabId,
    rev: 1,
    config: { connectionId: "dev", database: "reporting", schema: null, mode: "Manual" },
    session: { kind: "Open", backendPid: 7 },
    txn: "Open",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
}

it("names the notebooks a connection takes with it, closes their cells, and then deletes it", async () => {
  const user = userEvent.setup();
  ws().upsertConfig(connectionConfig("dev", "Dev SQL Server"));
  ws().upsertConfig(connectionConfig("prod", "prod-postgres"));
  const kept = openSqlTab({ db: db("prod", "shop"), text: "select 1" });
  const drift = openSqlTab({ db: db("dev", "shop"), text: "select 2" });
  openSqlTab({ db: db("dev", "shop"), text: "select 3" });
  const nightly = openSqlTab({ db: db("dev", "reporting"), text: "begin" });
  for (const [cell, title] of [
    [drift, "Schema drift"],
    [nightly, "Nightly load check"],
  ] as const)
    ws().renameNotebook(Object.values(ws().notebooks).find((n) => n.cells.includes(cell))!.id, title);
  useLive.setState({ consoles: { [nightly]: openTransaction(nightly) } });
  render(<Overlays />);
  let deleting: Promise<void> | undefined;
  act(() => {
    deleting = confirmDelete("dev");
  });
  const dialog = await screen.findByRole("alertdialog", { name: "Delete Dev SQL Server?" });
  expect(dialog.textContent).toContain("Removes the connection, its password, and these notebooks:");
  expect(
    within(screen.getByTestId("confirm-rows"))
      .getAllByRole("listitem")
      .map((row) => [...row.children].map((c) => c.textContent)),
  ).toEqual([
    ["Nightly load check", "reporting · 1 cell"],
    ["Schema drift", "shop · 2 cells"],
  ]);
  expect(dialog.textContent).toContain("Any open transactions are rolled back.");
  expect(document.activeElement).toBe(screen.getByTestId("confirm-cancel"));
  await user.click(screen.getByTestId("confirm-btn"));
  await deleting;
  expect([Object.values(ws().notebooks).map((n) => n.title), allCells(ws()), calls]).toEqual([["shop"], [kept], ["close true", "delete"]]);
  await waitFor(() => expect(ws().configs.dev).toBeUndefined());
});

it("says only the connection and its password go when it has no notebooks", async () => {
  ws().upsertConfig(connectionConfig("dev", "Dev SQL Server"));
  render(<Overlays />);
  act(() => void confirmDelete("dev"));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete Dev SQL Server?" });
  expect(dialog.textContent).toContain("Removes the connection and its password.");
  expect(screen.queryByTestId("confirm-rows")).toBeNull();
  expect(dialog.textContent).not.toContain("rolled back");
});
