// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { Composer } from "./Composer";
import { cellsState } from "../../testing/notebooks";

afterEach(() => {
  cleanup();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

it("shows schema access and only offers query permissions and attachments in Full access", async () => {
  const user = userEvent.setup();
  const db = { connectionId: "c", database: "shop" };
  useWorkspace.setState(cellsState([{ id: "t", title: "orders.sql", console: { db, mode: "AutoCommit" }, table: null }]));
  useLive.setState({
    consoles: {
      t: {
        tabId: "t",
        rev: 1,
        config: { ...db, schema: null, mode: "AutoCommit" },
        session: { kind: "None" },
        txn: "Idle",
        txnDepth: null,
        work: null,
        export: null,
        import: null,
        run: {
          kind: "Finished",
          runId: 1,
          elapsedMs: 0,
          outcome: "Ok",
          statements: [
            {
              index: 0,
              range: { from: 0, to: 1 },
              status: { kind: "Finished" },
              outputs: [
                {
                  kind: "Rows",
                  state: "Complete",
                  result: {
                    ref: { tabId: "t", runId: 1, index: 0 },
                    db,
                    databaseKind: "Postgres",
                    columns: [],
                    rowCount: 1,
                    rowsSeen: 1,
                    truncated: null,
                    elapsedMs: 0,
                    allRows: { kind: "Statement" },
                    repeatedRead: null,
                  },
                },
              ],
            },
          ],
        },
      },
    },
  });
  const chat = {
    id: "chat",
    rev: 1,
    title: "test",
    updatedAtMs: 1,
    entryCount: 0,
    options: { provider: "Claude", model: "test", effort: null, permission: "Auto" },
    status: { kind: "Idle" },
  } as const;
  render(<Composer chat={chat} />);
  expect(screen.getByTestId("assistant-access").textContent).toContain("including anything you paste");
  expect(screen.queryByTestId("assistant-attach")).toBeNull();
  expect(screen.queryByTestId("assistant-permission")).toBeNull();
  act(() => useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "Full" } })));
  await user.click(screen.getByTestId("assistant-options"));
  expect(screen.getByTestId("assistant-permission-Auto").textContent).toContain("Auto");
  await user.click(screen.getByTestId("assistant-attach"));
  expect(screen.getByTestId("assistant-attachment")).toBeTruthy();
  act(() => useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "SchemaOnly" } })));
  expect(screen.queryByTestId("assistant-attachment")).toBeNull();
  act(() => useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "Full" } })));
  expect(screen.queryByTestId("assistant-attachment")).toBeNull();
});

it("follows the shown notebook's active cell, including an unconfigured cell and the start commands", () => {
  const consoleFor = (database: string) => ({ db: { connectionId: "c", database }, mode: "AutoCommit" as const });
  useWorkspace.setState({
    configs: { c: connectionConfig("c", "Dev Postgres") },
    ...cellsState([
      { id: "a", title: "first.sql", console: consoleFor("shop"), table: null },
      { id: "b", title: "second.sql", console: consoleFor("reporting"), table: null },
      { id: "empty", title: "empty.sql", console: { db: null, mode: "AutoCommit" }, table: null },
    ]),
  });
  render(<Composer chat={undefined} />);
  expect(screen.getByTestId("assistant-target").title).toBe("Dev Postgres / shop");
  act(() => useWorkspace.getState().setActive("b"));
  expect(screen.getByTestId("assistant-target").textContent).toBe("reporting / default");
  act(() => useWorkspace.getState().setActive("empty"));
  expect(screen.getByTestId("assistant-target").textContent).toBe("No database selected");
  act(() => useWorkspace.getState().showDatabase({ connectionId: "c", database: "archive" }));
  expect(screen.getByTestId("assistant-target").textContent).toBe("No database selected");
});

it("says per connection whether SQL Server queries are off or always ask", async () => {
  const user = userEvent.setup();
  const db = { connectionId: "c", database: "shop" };
  const sqlServer = { ...connectionConfig("c", "Shop"), databaseKind: "SqlServer" } as const;
  useWorkspace.setState({
    ...cellsState([{ id: "t", title: "orders.sql", console: { db, mode: "AutoCommit" }, table: null }]),
    configs: { c: sqlServer },
  });
  useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "Full" } }));
  const chat = {
    id: "chat",
    rev: 1,
    title: "test",
    updatedAtMs: 1,
    entryCount: 0,
    options: { provider: "Claude", model: "test", effort: null, permission: "Auto" },
    status: { kind: "Idle" },
  } as const;
  render(<Composer chat={chat} />);
  const hints = async () => {
    await user.click(screen.getByTestId("assistant-options"));
    const text = ["AskFirst", "Auto"].map((p) => screen.getByTestId(`assistant-permission-${p}`).textContent);
    await user.keyboard("{Escape}");
    return text;
  };
  expect(screen.getByTestId("assistant-sqlserver-limit").textContent).toContain("Turn on read-only queries");
  expect(await hints()).toEqual(["Ask firstQueries are off for this connection", "AutoQueries are off for this connection"]);

  act(() => useWorkspace.setState({ configs: { c: { ...sqlServer, assistantReadOnly: true } } }));
  expect(screen.getByTestId("assistant-sqlserver-limit").textContent).toContain("every query asks first");
  expect(await hints()).toEqual(["Ask firstEvery query waits for Run or Deny", "AutoSQL Server queries still ask first"]);
});
