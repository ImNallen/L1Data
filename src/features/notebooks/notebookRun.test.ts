import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { NotebookId, TabId } from "../../ipc/bindings";
import { connectionConfig } from "../../testing/connections";
import { fakeEngine, RUN_DB as db } from "../../testing/runEngine";
import { useLive } from "../../state/live";
import { notebookOf } from "../../state/notebooks";
import { activeId, useWorkspace } from "../../state/workspace";
import { dropEditor, seedEditor } from "../editor/editorStates";
import { finishParams, useParams } from "../editor/paramsState";
import { forgetTab, openSqlTab } from "../editor/runActions";
import { notebookRunning, runNotebook, stopNotebookRun } from "./notebookRun";

const ws = () => useWorkspace.getState();

function notebook(...texts: string[]): { id: NotebookId; cells: TabId[] } {
  const cells = texts.map((text, i) => openSqlTab({ db, text, title: `cell ${i + 1}` }));
  return { id: notebookOf(ws().notebooks, cells[0]!)!.id, cells };
}

const question = () => {
  const overlay = ws().overlay;
  return overlay.kind === "ask" ? overlay : null;
};

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  useWorkspace.setState({ configs: { prod: { ...connectionConfig("prod", "prod-postgres"), environment: "Production" } } });
});

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useParams.setState({ prompt: null });
});

describe("a notebook run", () => {
  it("asks for every missing value once, asks one Production question, then runs each cell in order", async () => {
    const engine = fakeEngine({ writes: ["insert into t values (1)", "update t set n = :x"] });
    const nb = notebook("insert into t values (1)", "select :x::int", "  ", "update t set n = :x", "select 4");
    const done = runNotebook(nb.id, "all");

    await vi.waitFor(() => expect(useParams.getState().prompt?.params).toEqual([{ name: ":x" }]));
    expect(useParams.getState().prompt?.description).toBe("prod-postgres / shop · 4 cells");
    finishParams([{ name: ":x", value: "7" }]);
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      title: "Run 2 writes on Production?",
      rows: [
        { label: "cell 1", detail: "1 write" },
        { label: "cell 4", detail: "1 write" },
      ],
    });
    expect(engine.sent).toEqual([]);
    ws().answer("btn");
    await done;

    expect(engine.sent.map(({ text, confirmed }) => [text, confirmed])).toEqual([
      ["insert into t values (1)", "ok:insert into t values (1)"],
      ["select :x::int", null],
      ["update t set n = :x", "ok:update t set n = :x"],
      ["select 4", null],
    ]);
    expect(ws().notebooks[nb.id]?.params).toEqual([{ name: ":x", value: "7" }]);
    expect(ws().toast?.text).toBe("Ran 4 cells.");
    expect(notebookRunning(nb.id)).toBe(false);
  });

  it("runs nothing when the prompt or the question is cancelled", async () => {
    const engine = fakeEngine({ writes: ["insert into t values (1)"] });
    const nb = notebook("select :x::int", "insert into t values (1)");
    let done = runNotebook(nb.id, "all");
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    finishParams(null);
    await done;
    expect(engine.checked).toEqual([]);

    done = runNotebook(nb.id, "all");
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    finishParams([{ name: ":x", value: "1" }]);
    await vi.waitFor(() => expect(question()).not.toBeNull());
    ws().close();
    await done;
    expect(engine.sent).toEqual([]);
  });

  it("stops at the first failed cell, makes it active without expanding it, and names it", async () => {
    const engine = fakeEngine({ outcomes: { "select 1/0": "Failed" } });
    const nb = notebook("select 1", "select 2", "select 1/0", "select 4");
    ws().setCollapsed([nb.cells[2]!], true);
    await runNotebook(nb.id, "all");
    expect(engine.texts()).toEqual(["select 1", "select 2", "select 1/0"]);
    expect(ws().toast?.text).toBe("Stopped at cell 3: division by zero");
    expect(activeId(ws())).toBe(nb.cells[2]);
    expect(ws().tabs[nb.cells[2]!]?.collapsed).toBe(true);
  });

  it("stops at a cell the engine refuses", async () => {
    const engine = fakeEngine({ refuse: { "select 2": { kind: "Busy" } } });
    const nb = notebook("select 1", "select 2", "select 3");
    await runNotebook(nb.id, "all");
    expect(engine.texts()).toEqual(["select 1", "select 2"]);
    expect(ws().toast?.text).toBe("Stopped at cell 2: This tab is still running. Cancel it first.");
  });

  it("stops when the running cell closes", async () => {
    const engine = fakeEngine({ outcomes: { "select 2": "hold" } });
    const nb = notebook("select 1", "select 2", "select 3");
    const done = runNotebook(nb.id, "all");
    await vi.waitFor(() => expect(engine.texts()).toContain("select 2"));
    forgetTab(nb.cells[1]!);
    await done;
    expect(engine.texts()).toEqual(["select 1", "select 2"]);
    expect(ws().toast?.text).toBe("cell 2 was closed.");
  });

  it("cancels the running cell on Stop and skips the rest", async () => {
    const engine = fakeEngine({ outcomes: { "select pg_sleep(5)": "hold" } });
    const nb = notebook("select 1", "select pg_sleep(5)", "select 3", "select 4");
    const done = runNotebook(nb.id, "all");
    await vi.waitFor(() => expect(useLive.getState().consoles[nb.cells[1]!]?.run.kind).toBe("Running"));
    stopNotebookRun(nb.id);
    await done;
    expect(engine.cancelled).toEqual([nb.cells[1]]);
    expect(engine.texts()).toEqual(["select 1", "select pg_sleep(5)"]);
    expect(ws().toast?.text).toBe("Stopped. 3 cells did not run.");
  });

  it("asks a cell's own question when it changed after the run's question", async () => {
    const engine = fakeEngine({ writes: ["insert into t values (1)", "insert into t values (2)"] });
    const nb = notebook("select 1", "insert into t values (1)");
    const done = runNotebook(nb.id, "all");
    await vi.waitFor(() => expect(question()?.title).toBe("Run 1 write on Production?"));
    seedEditor(nb.cells[1]!, "insert into t values (2)");
    ws().answer("btn");
    await vi.waitFor(() => expect(question()?.body).toBe("This run sends 1 statement that writes to prod-postgres / shop."));
    ws().answer("btn");
    await done;
    expect(engine.sent.slice(1).map((s) => [s.text, s.confirmed])).toEqual([
      ["insert into t values (2)", "ok:insert into t values (1)"],
      ["insert into t values (2)", "ok:insert into t values (2)"],
    ]);
  });

  it("runs the stale cells, or the active cell and those after it", async () => {
    const engine = fakeEngine();
    const nb = notebook("select 1", "select 2", "select 3");
    ws().setRunSources(nb.cells[2]!, [{ name: "cell 1", text: "select 0" }]);
    await runNotebook(nb.id, "stale");
    expect(engine.texts()).toEqual(["select 3"]);

    ws().setActive(nb.cells[1]!);
    await runNotebook(nb.id, "fromActive");
    expect(engine.texts()).toEqual(["select 3", "select 2", "select 3"]);
    await runNotebook(nb.id, "stale");
    expect(ws().toast?.text).toBe("No cell is stale.");
  });

  it("leaves a running notebook alone", async () => {
    const engine = fakeEngine({ outcomes: { "select 1": "hold" } });
    const nb = notebook("select 1", "select 2");
    const first = runNotebook(nb.id, "all");
    await vi.waitFor(() => expect(engine.texts()).toEqual(["select 1"]));
    await runNotebook(nb.id, "all");
    expect(engine.texts()).toEqual(["select 1"]);
    stopNotebookRun(nb.id);
    await first;
  });
});
