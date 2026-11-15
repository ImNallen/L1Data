import { mockIPC } from "@tauri-apps/api/mocks";
import type { ConsoleState, RunError, RunOutcome, RunRequest, TabId, WriteConfirmation } from "../ipc/bindings";
import { useLive } from "../state/live";

export const RUN_DB = { connectionId: "prod", database: "shop" };

const asks = (text: string, digest = `ok:${text}`): WriteConfirmation => ({
  connection: "prod-postgres",
  database: "shop",
  unit: "Statement",
  sends: { count: 1, statements: [text] },
  holds: { count: 0, statements: [] },
  digest,
  onProduction: true,
});

type Sent = { tabId: TabId; text: string; confirmed: string | null };

/**
 * The engine as the runner sees it. A run's outcome comes from `outcomes` by cell text, Ok by default; "hold" keeps it
 * running until a cancel. A check asks for every text in `writes`, and a run of one asks until it gets that digest.
 */
export function fakeEngine(opts: { writes?: string[]; outcomes?: Record<string, RunOutcome | "hold">; refuse?: Record<string, RunError> } = {}) {
  const sent: Sent[] = [];
  const checked: string[] = [];
  const cancelled: TabId[] = [];
  const held = new Map<TabId, number>();
  let rev = 0;
  let runId = 0;
  const publish = (tabId: TabId, run: ConsoleState["run"]) =>
    useLive.getState().apply({
      kind: "ConsoleChanged",
      tabId,
      rev: ++rev,
      config: { ...RUN_DB, schema: null, mode: "AutoCommit" },
      session: { kind: "None" },
      txn: "Idle",
      txnDepth: null,
      work: null,
      export: null,
      import: null,
      run,
    });
  const finish = (tabId: TabId, id: number, outcome: RunOutcome) => {
    const statements =
      outcome === "Failed"
        ? [
            {
              index: 0,
              range: { from: 0, to: 1 },
              outputs: [],
              status: {
                kind: "Failed" as const,
                error: { severity: "ERROR", sqlstate: "22012", message: "division by zero", detail: null, hint: null, context: null, position: null },
              },
            },
          ]
        : [];
    publish(tabId, { kind: "Finished", runId: id, elapsedMs: 1, statements, outcome });
  };
  mockIPC((cmd, args) => {
    const { tabId, request, confirmed, text } = args as {
      tabId: TabId;
      request: Extract<RunRequest, { kind: "script" }>;
      confirmed: string | null;
      text: string;
    };
    switch (cmd) {
      case "script_params":
        return text.includes(":x") ? [{ name: ":x" }] : [];
      case "console_check_run":
        checked.push(request.text);
        return opts.writes?.includes(request.text) ? asks(request.text) : null;
      case "console_run": {
        sent.push({ tabId, text: request.text, confirmed });
        const refusal = opts.refuse?.[request.text];
        if (refusal) return Promise.reject(refusal);
        if (opts.writes?.includes(request.text) && confirmed !== `ok:${request.text}`)
          return Promise.reject({ kind: "ConfirmWrites", confirmation: asks(request.text) } satisfies RunError);
        const id = ++runId;
        const outcome = opts.outcomes?.[request.text] ?? "Ok";
        setTimeout(() => {
          publish(tabId, { kind: "Running", runId: id, startedAtMs: 0, statements: [] });
          if (outcome === "hold") held.set(tabId, id);
          else finish(tabId, id, outcome);
        }, 0);
        return { runId: id, statements: [], sources: [] };
      }
      case "console_cancel": {
        cancelled.push(tabId);
        const id = held.get(tabId);
        if (id !== undefined) finish(tabId, id, "Cancelled");
        return null;
      }
      default:
        return null;
    }
  });
  return { sent, checked, cancelled, texts: () => sent.map((s) => s.text) };
}
