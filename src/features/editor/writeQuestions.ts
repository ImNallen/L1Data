import type { ExecutionUnitKind, RunMode, WriteConfirmation } from "../../ipc/bindings";
import type { confirm } from "../../state/workspace";

export type WriteQuestion = Parameters<typeof confirm>[0];

export const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

function units(unit: ExecutionUnitKind, n: number): string {
  return unit === "Batch" ? `${n} ${n === 1 ? "batch" : "batches"}` : plural(n, "statement");
}

function sendsWrites(asked: WriteConfirmation, n: number): string {
  return `This run sends ${units(asked.unit, n)} that ${n === 1 ? "writes" : "write"} to ${asked.connection} / ${asked.database}.`;
}

/** A held write may still be open or may have committed already, and a run may be in either mode, so each sentence must hold for all of them. */
function held(asked: WriteConfirmation, count: number, where: { on: string; in: string }, outcome: (it: string) => string): string {
  const one = count === 1;
  return [
    `${one ? "A write" : `${count} writes`} to ${asked.connection} / ${asked.database} ran ${where.on} before the connection was marked Production.`,
    `If ${one ? "it is" : "they are"} still in ${where.in}, ${outcome(one ? "it" : "them")}.`,
  ].join(" ");
}

function covered(it: string): string {
  return `confirming covers ${it}, so a later commit does not ask about ${it} again`;
}

export function heldWrites(asked: WriteConfirmation, outcome: (it: string) => string = covered): string {
  return held(asked, asked.holds.count, { on: "on this tab", in: "this tab's transaction" }, outcome);
}

export function shownWrites(asked: WriteConfirmation): { statements: string[]; note: string } {
  const statements = [...asked.holds.statements, ...asked.sends.statements];
  const total = asked.holds.count + asked.sends.count;
  return { statements, note: total > statements.length ? `${statements.length} of ${total} are shown.` : "" };
}

export function runQuestion(asked: WriteConfirmation, mode: RunMode, sqlServer: boolean): WriteQuestion {
  const n = asked.sends.count;
  const { statements, note } = shownWrites(asked);
  const sqlServerAnalyze = mode === "ExplainAnalyze" && sqlServer;
  const offProduction = sqlServerAnalyze && !asked.onProduction;
  const body = [
    offProduction
      ? `Explain Analyze executes this batch on ${asked.connection} / ${asked.database}. This app does not roll it back.`
      : n > 0 && sendsWrites(asked, n),
    asked.holds.count > 0 && heldWrites(asked),
    note,
    mode === "ExplainAnalyze" && !sqlServerAnalyze && "A rollback does not undo sequence values or outside side effects.",
  ];
  return {
    title: offProduction ? "Run this batch?" : n > 0 ? `Run ${plural(n, "write")} on Production?` : "Run on Production?",
    body: body.filter(Boolean).join(" "),
    statements,
    confirmLabel: offProduction ? "Run" : n > 0 ? "Run writes" : "Run",
  };
}
