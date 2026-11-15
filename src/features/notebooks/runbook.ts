import type { NotebookId, Param, TabId } from "../../ipc/bindings";
import { runErrorText } from "../../ipc/errors";
import type { Ending } from "../../state/live";
import type { Sent } from "../editor/runActions";
import { plural } from "../editor/writeQuestions";

/**
 * A notebook run from its first step to its last: it prepares (the prompt and the question), runs cell `at` of `cells`,
 * and once stopping never runs another cell.
 */
export type NotebookRun = { notebook: NotebookId; cells: readonly TabId[]; at: number; phase: "preparing" | "running" | "stopping" };

export type RunScope = "all" | "stale" | "fromActive";

/** `runnable`: not blank, and not a table cell browsing its table. */
export type CellFacts = { id: TabId; runnable: boolean; stale: boolean };

/** The runnable cells in notebook order: all of them, the stale ones, or the active one and every one after it. */
export function chooseCells(cells: readonly CellFacts[], scope: RunScope, active: TabId | null): TabId[] {
  const from = scope === "fromActive" ? cells.findIndex((c) => c.id === active) : 0;
  if (from === -1) return [];
  return cells
    .slice(from)
    .filter((c) => c.runnable && (scope !== "stale" || c.stale))
    .map((c) => c.id);
}

/**
 * The last index the run's one prompt and one question cover: the first cell whose SQL cannot be prepared (`null`),
 * since the run is certain to stop there, or else the last cell.
 */
export function horizon(found: readonly (readonly Param[] | null)[]): number {
  const broken = found.indexOf(null);
  return broken === -1 ? found.length - 1 : broken;
}

type CellResult = Exclude<Sent, { kind: "accepted" }> | Ending;

type Verdict = { kind: "next" } | { kind: "skipped" } | { kind: "stopped" } | { kind: "dropped" } | { kind: "failed"; notice: string; reveal: boolean };

/**
 * What the run does after a cell. An Ok cell lets it go on, and a cell with only comments is skipped as a note. After
 * Stop, anything else is the stop. A dropped send stops it quietly, since the user said no or the reason is already shown. A closed cell stops it without showing the
 * cell. A refusal, a failure, or a cancel the user made in the cell stops it at that cell.
 */
export function afterCell(title: string, result: CellResult, stopping: boolean): Verdict {
  if (result.kind === "finished" && result.run.outcome === "Ok") return { kind: "next" };
  if (stopping) return { kind: "stopped" };
  if (result.kind === "refused" && result.error.kind === "NoStatement") return { kind: "skipped" };
  switch (result.kind) {
    case "dropped":
      return { kind: "dropped" };
    case "gone":
      return { kind: "failed", notice: `${title} was closed.`, reveal: false };
    case "refused":
      return { kind: "failed", notice: `Stopped at ${title}: ${runErrorText(result.error)}`, reveal: true };
    case "finished": {
      if (result.run.outcome === "Cancelled") return { kind: "failed", notice: `${title} was cancelled.`, reveal: true };
      const failed = result.run.statements.find((s) => s.status.kind === "Failed");
      const reason = failed?.status.kind === "Failed" ? `: ${failed.status.error.message}` : ".";
      return { kind: "failed", notice: `Stopped at ${title}${reason}`, reveal: true };
    }
  }
}

export const ranNotice = (ran: number) => `Ran ${plural(ran, "cell")}.`;

export const stoppedNotice = (left: number) => `Stopped. ${plural(left, "cell")} did not run.`;

/** Which cell of how many runs now, such as "2 of 5". Nothing while the run prepares or stops. */
export function progressText(run: NotebookRun): string {
  return run.phase === "running" ? `${run.at + 1} of ${run.cells.length}` : "";
}
