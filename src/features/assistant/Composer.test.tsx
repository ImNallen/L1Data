// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { Composer } from "./Composer";

afterEach(() => {
  cleanup();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

it("shows schema access and only offers query permissions and attachments in Full access", async () => {
  const user = userEvent.setup();
  const db = { connectionId: "c", database: "shop" };
  useWorkspace.setState({ active: "t", tabs: { t: { id: "t", title: "orders.sql", console: { db, mode: "AutoCommit" }, table: null, file: null } } });
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
  expect(screen.getByTestId("assistant-permission").textContent).toContain("Auto");
  await user.click(screen.getByTestId("assistant-attach"));
  expect(screen.getByTestId("assistant-attachment")).toBeTruthy();
  act(() => useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "SchemaOnly" } })));
  expect(screen.queryByTestId("assistant-attachment")).toBeNull();
  act(() => useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "Full" } })));
  expect(screen.queryByTestId("assistant-attachment")).toBeNull();
});

it("follows the active tab's database, including an unconfigured tab and no tab", () => {
  const consoleFor = (database: string) => ({ db: { connectionId: "c", database }, mode: "AutoCommit" as const });
  useWorkspace.setState({
    active: "a",
    configs: { c: connectionConfig("c", "Dev Postgres") },
    tabs: {
      a: { id: "a", title: "first.sql", console: consoleFor("shop"), table: null, file: null },
      b: { id: "b", title: "second.sql", console: consoleFor("reporting"), table: null, file: null },
      empty: { id: "empty", title: "empty.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null },
    },
  });
  render(<Composer chat={undefined} />);
  expect(screen.getByTestId("assistant-target").title).toBe("Dev Postgres / shop");
  act(() => useWorkspace.setState({ active: "b" }));
  expect(screen.getByTestId("assistant-target").textContent).toBe("Dev Postgres / reporting");
  act(() => useWorkspace.setState({ active: "empty" }));
  expect(screen.getByTestId("assistant-target").textContent).toBe("No database selected");
  act(() => useWorkspace.setState({ active: null }));
  expect(screen.getByTestId("assistant-target").textContent).toBe("No database selected");
});
