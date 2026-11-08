// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { ConsoleState, RunState, StatementRun } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace, type Tab } from "../../state/workspace";
import { ResultsPane } from "./ResultsPane";
import { cellsState } from "../../testing/notebooks";

const db = { connectionId: "c1", database: "shop" };
const tab = (id: string): Tab => ({ id, title: "Query", console: { db, mode: "AutoCommit" }, table: null, file: null });
const update: StatementRun = {
  index: 0,
  range: { from: 0, to: 1 },
  status: { kind: "Finished" },
  outputs: [{ kind: "Command", tag: "UPDATE 3", elapsedMs: 4, affectedRows: 3 }],
};
const error = { severity: "ERROR", sqlstate: "42P01", message: 'relation "nope" does not exist', detail: null, hint: null, context: null, position: 14 };
const failed: StatementRun = { index: 0, range: { from: 0, to: 1 }, status: { kind: "Failed", error }, outputs: [] };

const running: RunState = { kind: "Running", runId: 1, startedAtMs: Date.now(), statements: [] };
const finished = (statements: StatementRun[], outcome: "Ok" | "Failed" = "Ok"): Extract<RunState, { kind: "Finished" }> => ({
  kind: "Finished",
  runId: 1,
  elapsedMs: 5,
  statements,
  outcome,
});

function setRun(tabId: string, run: RunState) {
  const state: ConsoleState = {
    tabId,
    rev: 1,
    config: { ...db, schema: null, mode: "AutoCommit" },
    session: { kind: "None" },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run,
  };
  act(() => useLive.setState((s) => ({ consoles: { ...s.consoles, [tabId]: state } })));
}

beforeEach(() => {
  useWorkspace.setState(cellsState([tab("t1")]));
});

afterEach(() => {
  cleanup();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

const announced = () => screen.getByRole("status").textContent;

it("announces a finished run once it settles, and stays quiet while it runs", () => {
  setRun("t1", running);
  render(<ResultsPane tabId="t1" />);
  expect(screen.getByTestId("result-status").textContent).toContain("Running");
  expect(announced()).toBe("");

  setRun("t1", finished([update]));
  expect(announced()).toBe("UPDATE 3 · 4 ms");

  setRun("t1", running);
  expect(announced()).toBe("");
});

it("announces a failure with its error", () => {
  setRun("t1", running);
  render(<ResultsPane tabId="t1" />);
  setRun("t1", finished([failed], "Failed"));
  expect(announced()).toBe('Query failed: relation "nope" does not exist');
});

it("shows guidance only while idle and preserves the command, empty completion, running, and cancelled states", () => {
  render(<ResultsPane tabId="t1" />);
  expect(screen.getByText("Results appear here")).toBeTruthy();
  setRun("t1", running);
  expect(screen.queryByText("Results appear here")).toBeNull();
  setRun("t1", finished([update]));
  expect(screen.getByText("UPDATE 3")).toBeTruthy();
  expect(screen.queryByText("Results appear here")).toBeNull();
  setRun("t1", finished([]));
  expect(screen.getByText("The statement returned no rows.")).toBeTruthy();
  setRun("t1", { ...finished([]), outcome: "Cancelled" });
  expect(screen.getByText("Cancelled.")).toBeTruthy();
});
