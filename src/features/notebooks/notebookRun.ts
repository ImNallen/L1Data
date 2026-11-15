import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { commands, type NotebookId, type Param, type RunTarget, type TabId, type WriteDigest } from "../../ipc/bindings";
import { whenFinished } from "../../state/live";
import { confirm, shownNotebook, useWorkspace } from "../../state/workspace";
import { textOf } from "../editor/editorStates";
import { resolveParams } from "../editor/paramsState";
import { cancelCell, ensureConsole, isBrowsing, runCell, scriptRequest, textTarget } from "../editor/runActions";
import { plural, productionQuestion, type CellAsk } from "../editor/writeQuestions";
import { notebookSources, staleReason } from "./cellReferences";
import { afterCell, chooseCells, horizon, ranNotice, stoppedNotice, type NotebookRun, type RunScope } from "./runbook";

const ALL: RunTarget = { kind: "all" };
const ws = () => useWorkspace.getState();
const notify = (text: string) => ws().notify(text);

const useRuns = create<{ runs: Record<NotebookId, NotebookRun> }>(() => ({ runs: {} }));

export function useNotebookRun(id: NotebookId | undefined): NotebookRun | undefined {
  return useRuns((s) => (id ? s.runs[id] : undefined));
}

export function useRunningNotebooks(): readonly NotebookId[] {
  return useRuns(useShallow((s) => Object.keys(s.runs)));
}

export function notebookRunning(id: NotebookId): boolean {
  return id in useRuns.getState().runs;
}

const put = (run: NotebookRun) => useRuns.setState((s) => ({ runs: { ...s.runs, [run.notebook]: run } }));

/** The run goes on until Stop, or until its notebook closes. */
function live(run: NotebookRun): boolean {
  const now = useRuns.getState().runs[run.notebook];
  return now !== undefined && now.phase !== "stopping" && run.notebook in ws().notebooks;
}

function runnable(id: TabId): boolean {
  const tab = ws().tabs[id];
  return tab !== undefined && textOf(id).trim() !== "" && !isBrowsing(tab);
}

/**
 * Runs the notebook's chosen cells in notebook order, and stops at the first that does not finish Ok. One prompt asks
 * for every missing value and one question covers every cell that would ask on Production, before the first cell runs.
 * A notebook that is already running is left alone.
 */
export async function runNotebook(id: NotebookId, scope: RunScope): Promise<void> {
  const notebook = ws().notebooks[id];
  if (!notebook || notebookRunning(id)) return;
  const facts = notebook.cells.map((c) => ({ id: c, runnable: runnable(c), stale: staleReason(c) !== "" }));
  const cells = chooseCells(facts, scope, notebook.active);
  if (cells.length === 0) return notify(scope === "stale" ? "No cell is stale." : "No cell to run.");
  const run: NotebookRun = { notebook: id, cells, at: 0, phase: "preparing" };
  put(run);
  try {
    const digests = await preflight(run);
    if (digests) await sequence(run, digests);
  } finally {
    useRuns.setState((s) => {
      const { [id]: _ended, ...runs } = s.runs;
      return { runs };
    });
  }
}

export function stopNotebookRun(id: NotebookId): void {
  const run = useRuns.getState().runs[id];
  if (!run || run.phase === "stopping") return;
  put({ ...run, phase: "stopping" });
  const cell = run.cells[run.at];
  if (run.phase === "running" && cell) cancelCell(cell);
}

async function params(id: TabId): Promise<Param[] | null> {
  const tab = ws().tabs[id];
  if (!tab) return [];
  const res = await commands.scriptParams(textOf(id), ALL, textTarget(tab).databaseKind, notebookSources(id));
  return res.status === "ok" ? res.data : null;
}

function description(run: NotebookRun): string {
  const db = ws().notebooks[run.notebook]?.db;
  const where = db ? `${ws().configs[db.connectionId]?.name ?? db.connectionId} / ${db.database} · ` : "";
  return `${where}${plural(run.cells.length, "cell")}`;
}

/** The digest each cell may run with, once the user has answered for every cell the run can reach. `null` runs nothing. */
async function preflight(run: NotebookRun): Promise<Map<TabId, WriteDigest> | null> {
  const found = await Promise.all(run.cells.map(params));
  if (!live(run)) return null;
  const reach = run.cells.slice(0, horizon(found) + 1);
  const names = found.slice(0, reach.length).flatMap((f) => f ?? []);
  if (!(await resolveParams(run.notebook, names, description(run), () => live(run)))) return null;
  const asks: (CellAsk & { id: TabId })[] = [];
  for (const id of reach) {
    const tab = ws().tabs[id];
    if (!tab) continue;
    if (!(await ensureConsole(tab))) break;
    const res = await commands.consoleCheckRun(id, scriptRequest(id, textOf(id), ALL, "Run"));
    if (!live(run)) return null;
    if (res.status === "ok" && res.data) asks.push({ id, title: tab.title, confirmation: res.data });
  }
  const question = productionQuestion(asks);
  if (question && (!(await confirm(question)) || !live(run))) return null;
  return new Map(asks.map((a) => [a.id, a.confirmation.digest]));
}

async function sequence(run: NotebookRun, digests: Map<TabId, WriteDigest>): Promise<void> {
  let ran = 0;
  for (const [at, id] of run.cells.entries()) {
    if (!live(run)) return notify(stoppedNotice(run.cells.length - ran));
    const tab = ws().tabs[id];
    if (!tab || !runnable(id)) continue;
    put({ ...run, at, phase: "running" });
    const sent = await runCell(id, digests.get(id) ?? null, () => live(run));
    // Stop can land while the send is in flight, before the cell has a run to cancel.
    if (sent.kind === "accepted" && !live(run)) cancelCell(id);
    const verdict = afterCell(tab.title, sent.kind === "accepted" ? await whenFinished(id, sent.run.runId) : sent, !live(run));
    switch (verdict.kind) {
      case "next":
        ran++;
        continue;
      case "stopped":
        return notify(stoppedNotice(run.cells.length - ran));
      case "dropped":
        return;
      case "failed":
        notify(verdict.notice);
        if (verdict.reveal && shownNotebook(ws())?.id === run.notebook) ws().setActive(id);
        return;
    }
  }
  notify(ranNotice(ran));
}
