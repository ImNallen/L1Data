import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it } from "vitest";
import type { ConsoleState, ConsoleWork, DatabaseKind, Environment, TransactionLoss, TxnState } from "../ipc/bindings";
import { connectedState, connectionConfig, databaseState } from "../testing/connections";
import { useLive } from "../state/live";
import { useWorkspace, type Tab } from "../state/workspace";
import { TopBar, databaseChoices } from "./TopBar";
import { cellsState } from "../testing/notebooks";

const workspace = useWorkspace.getInitialState();
const live = useLive.getInitialState();
const saved = { tabs: workspace.tabs, notebooks: workspace.notebooks, shown: workspace.shown, configs: workspace.configs, consoles: live.consoles };

afterEach(() => {
  Object.assign(workspace, { tabs: saved.tabs, notebooks: saved.notebooks, shown: saved.shown, configs: saved.configs });
  live.consoles = saved.consoles;
});

function onConnection(environment: Environment | null, txn: TxnState = "Idle", databaseKind: DatabaseKind = "Postgres"): string {
  const tab: Tab = { id: "t1", title: "Query 3", console: { db: { connectionId: "c1", database: "shop" }, mode: "Manual" }, table: null, file: null };
  const state: ConsoleState = {
    tabId: tab.id,
    rev: 1,
    config: { connectionId: "c1", database: "shop", schema: null, mode: "Manual" },
    session: { kind: "Open", backendPid: 1 },
    txn,
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
  Object.assign(workspace, {
    ...cellsState([tab]),
    configs: { c1: { ...connectionConfig("c1", "prod-postgres"), environment, databaseKind } },
  });
  live.consoles = { [tab.id]: state };
  return renderToStaticMarkup(<TopBar hosted={null} />);
}

const header = (html: string) => html.slice(0, html.indexOf(">") + 1);
it.each(["Production", "Staging", "Local", null] satisfies (Environment | null)[])("keeps %s environment styling on the Run button only", (environment) => {
  const html = onConnection(environment);
  expect(html).not.toContain('data-testid="env-icon"');
  expect(header(html)).not.toContain("border-danger");
  expect(html).not.toContain("ring-danger");
  expect(html).toContain(`class="run-environment inline-flex"${environment ? ` data-environment="${environment}"` : ""}`);
});

it("reads the run target as the connection, then database, and leaves the tab title to the tab bar", () => {
  const html = onConnection("Production");
  const at = (pattern: RegExp) => html.search(pattern);
  const order = [at(/data-testid="tab-connection"/), at(/>prod-postgres</), at(/data-testid="tab-database"[^>]*>(<[^>]*>)*shop</)];
  expect(order.every((i) => i >= 0)).toBe(true);
  expect([...order].sort((a, b) => a - b)).toEqual(order);
  expect(html).not.toContain("Query 3");
});

it("offers Explain on a PostgreSQL or SQL Server script tab", () => {
  expect(onConnection("Local")).toContain('data-testid="explain-menu"');
  expect(onConnection("Local", "Idle", "SqlServer")).toContain('data-testid="explain-menu"');
});

it("shows an open transaction as pending and an aborted one as an error", () => {
  expect(onConnection(null, "Open")).toMatch(/<span class="text-amber"> · in transaction/);
  expect(onConnection(null, "Aborted")).toMatch(/<span class="text-danger"> · aborted/);
});

it("offers the shown databases and keeps the tab's own when the tree hides it or the connection is down", () => {
  const db = { connectionId: "c1", database: "shop" };
  const shown = connectedState("c1", [databaseState("reporting", true, []), databaseState("shop", true, []), databaseState("staging_copy", false, [])]);
  expect(databaseChoices(db, shown)).toEqual(["reporting", "shop"]);
  expect(databaseChoices({ ...db, database: "staging_copy" }, shown)).toEqual(["staging_copy", "reporting", "shop"]);
  expect(databaseChoices(db, undefined)).toEqual(["shop"]);
  expect(databaseChoices(db, { id: "c1", rev: 2, status: { kind: "Disconnected" } })).toEqual(["shop"]);
});

function lostSession(transaction: TransactionLoss): string {
  const tab: Tab = { id: "t1", title: "Query", console: { db: { connectionId: "c1", database: "shop" }, mode: "AutoCommit" }, table: null, file: null };
  const state: ConsoleState = {
    tabId: tab.id,
    rev: 1,
    config: { connectionId: "c1", database: "shop", schema: null, mode: "AutoCommit" },
    session: { kind: "Lost", reason: "cancelled", transaction },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
  Object.assign(workspace, cellsState([tab]));
  live.consoles = { [tab.id]: state };
  return renderToStaticMarkup(<TopBar hosted={null} />);
}

it("shows a session reset without promising rollback when the transaction outcome is unknown", () => {
  const html = lostSession("Unknown");
  expect(html).toContain("session reset");
  expect(html).toContain("Earlier commits remain committed; the last transaction outcome may be unknown.");
  expect(html).not.toContain("rolled back");
});

it("retains the confirmed PostgreSQL rollback message", () => {
  const html = lostSession("RolledBack");
  expect(html).toContain("rolled back");
  expect(html).toContain("Transaction rolled back: connection lost");
  expect(html).not.toContain("session reset");
});

it("does not claim a lost transaction when an idle PostgreSQL session closes", () => {
  const html = lostSession("None");
  expect(html).not.toContain("rolled back");
  expect(html).not.toContain("session reset");
});

it("shows an open transaction without claiming its depth or committability", () => {
  lostSession("None");
  const state = live.consoles.t1!;
  live.consoles = { t1: { ...state, session: { kind: "Open", backendPid: 1 }, txn: "Open", txnDepth: null } };
  const html = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(html).toContain("in transaction");
  expect(html).toContain("does not report nesting depth or committability");
  expect(html).toContain('data-testid="commit-btn"');
  expect(html).toContain('data-testid="rollback-btn"');
  expect(html).not.toContain("aborted");
  expect(html).not.toContain("in transaction (1)");
});

function workingConsole(work: ConsoleWork): string {
  lostSession("None");
  const state = live.consoles.t1!;
  live.consoles = {
    t1: {
      ...state,
      work,
      session: { kind: "Open", backendPid: 1 },
      txn: "Open",
      run: { kind: "Finished", runId: 7, elapsedMs: 1, outcome: "Ok", statements: [] },
    },
  };
  return renderToStaticMarkup(<TopBar hosted={null} />);
}

it("shows Counting and Cancel with Esc while retaining the finished run", () => {
  const html = workingConsole("Count");
  expect(html).toContain("Counting");
  expect(html).toContain('data-testid="cancel-btn"');
  expect(html).toContain("Esc");
  expect(html).not.toContain('data-testid="run-btn"');
  expect(html).toMatch(/data-testid="commit-btn"[^>]*disabled/);
  expect(html).toMatch(/data-testid="rollback-btn"[^>]*disabled/);
  expect(live.consoles.t1?.run.kind).toBe("Finished");
});

it("offers Cancel with Esc while an export streams", () => {
  const html = workingConsole("Export");
  expect(html).toContain('data-testid="cancel-btn"');
  expect(html).toContain("Esc");
  expect(html).not.toContain('data-testid="run-btn"');
  expect(html).toMatch(/data-testid="commit-btn"[^>]*disabled/);
});

it.each(["Transaction", "Edit", "Probe"] satisfies ConsoleWork[])("keeps %s ownership busy without offering Cancel", (work) => {
  const html = workingConsole(work);
  expect(html).not.toContain('data-testid="cancel-btn"');
  expect(html).not.toContain("Counting");
  expect(html).toMatch(/data-testid="run-btn"[^>]*disabled/);
});
