import type { ConnectionState, ConsoleState, OutputEntry, ResultMeta, RowsState, RunState, StatementRun } from "../ipc/bindings";
import { formatElapsed, groupDigits, pluralRows } from "../features/results/format";

export function hasUncommittedWork(console: ConsoleState | undefined): boolean {
  return console !== undefined && console.txn !== "Idle";
}

export function isRunning(console: ConsoleState | undefined): boolean {
  return console?.run.kind === "Running";
}

export function isBusy(console: ConsoleState | undefined): boolean {
  return console?.work != null || isRunning(console);
}

export function canCancel(console: ConsoleState | undefined): boolean {
  return console?.work === "Count" || console?.work === "Export" || console?.work === "Import" || isRunning(console);
}

export type Tone = "live" | "pending" | "off" | "error";

export function connectionTone(state: ConnectionState | undefined): Tone {
  const status = state?.status ?? { kind: "Disconnected" as const };
  switch (status.kind) {
    case "Connected":
      return "live";
    case "Connecting":
      return "pending";
    case "Disconnected":
      return "off";
    case "Failed":
    case "Lost":
      return "error";
    default: {
      const unreachable: never = status;
      return unreachable;
    }
  }
}

export type RowResult = { index: number; meta: ResultMeta; state: RowsState; plan: boolean };

export function runStatements(run: RunState): StatementRun[] {
  return run.kind === "Idle" ? [] : run.statements;
}

export function rowResults(run: RunState): RowResult[] {
  const out: RowResult[] = [];
  for (const s of runStatements(run)) {
    for (const output of s.outputs) {
      if (output.kind === "Rows") out.push({ index: output.result.ref.index, meta: output.result, state: output.state, plan: false });
      else if (output.kind === "Plan") out.push({ index: output.result.ref.index, meta: output.result, state: "Complete", plan: true });
    }
  }
  return out;
}

export type ResultPick = number | { kind: "output"; runId: number; result: number | undefined };

export function shownResult(run: RunState, picked: ResultPick | undefined): RowResult | undefined {
  const results = rowResults(run);
  const index = typeof picked !== "object" ? picked : run.kind !== "Idle" && picked.runId === run.runId ? picked.result : undefined;
  return results.find((r) => r.index === index) ?? results.at(-1);
}

export type ResultView =
  | { kind: "rows"; result: RowResult }
  | { kind: "plan"; result: RowResult }
  | { kind: "chart"; result: RowResult }
  | { kind: "output" }
  | { kind: "tail" };

export function resultKey(result: RowResult): string {
  return `${result.meta.ref.tabId}:${result.meta.ref.runId}:${result.index}`;
}

/** How the shown result asks to be drawn. "default" is the plan for a plan result, else rows. */
export type ResultDisplay = "default" | "rows" | "chart";

export function resultView(run: RunState, picked: ResultPick | undefined, hasOutput: boolean, display: ResultDisplay): ResultView {
  const pickedOutput = typeof picked === "object" && run.kind !== "Idle" && picked.runId === run.runId;
  if (hasOutput && pickedOutput) return { kind: "output" };
  const result = shownResult(run, picked);
  if (result?.plan && display !== "rows") return { kind: "plan", result };
  if (result && display === "chart") return { kind: "chart", result };
  if (result) return { kind: "rows", result };
  return hasOutput ? { kind: "output" } : { kind: "tail" };
}

export function outputTone(entries: readonly OutputEntry[]): "muted" | "warn" {
  const latest = entries.at(-1)?.runId;
  const warned = entries.some((e) => e.runId === latest && e.body.kind === "Notice" && e.body.severity === "WARNING");
  return warned ? "warn" : "muted";
}

export type Summary = { tone: "ok" | "error" | "running" | "muted"; text: string };

function rowsText(meta: ResultMeta): string {
  if (meta.truncated === null) return pluralRows(meta.rowCount);
  const why = meta.truncated === "RowLimit" ? "row limit" : "size limit";
  return `${groupDigits(String(meta.rowCount))} of ${groupDigits(String(meta.rowsSeen))} rows (${why})`;
}

export function runSummary(run: RunState, picked: ResultPick | undefined, nowMs: number): Summary | null {
  switch (run.kind) {
    case "Idle":
      return null;
    case "Running": {
      const elapsed = formatElapsed(Math.max(0, nowMs - run.startedAtMs));
      const streaming = shownResult(run, picked);
      return {
        tone: "running",
        text: streaming ? `${pluralRows(streaming.meta.rowCount)} · ${elapsed}` : `Running · ${elapsed}`,
      };
    }
    case "Finished": {
      const failed = run.statements.find((s) => s.status.kind === "Failed");
      if (failed?.status.kind === "Failed") return { tone: "error", text: failed.status.error.message };
      if (run.outcome === "Cancelled") return { tone: "muted", text: `Cancelled · ${formatElapsed(run.elapsedMs)}` };
      const shown = shownResult(run, picked);
      if (shown?.plan) return { tone: "ok", text: `Plan · ${formatElapsed(shown.meta.elapsedMs)}` };
      if (shown) return { tone: "ok", text: `${rowsText(shown.meta)} · ${formatElapsed(shown.meta.elapsedMs)}` };
      const last = run.statements.flatMap((s) => s.outputs).at(-1);
      if (last?.kind === "Command") return { tone: "ok", text: `${last.tag} · ${formatElapsed(last.elapsedMs)}` };
      return { tone: "ok", text: formatElapsed(run.elapsedMs) };
    }
    default: {
      const unreachable: never = run;
      return unreachable;
    }
  }
}
