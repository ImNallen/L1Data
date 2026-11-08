import { open } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";
import { commands, type ImportPreview, type ImportSourceId, type ImportTarget } from "../../ipc/bindings";
import { importErrorText } from "../../ipc/errors";
import { useLive } from "../../state/live";
import { NO_EDITS, pendingCount, useTableEdits } from "../../state/tableEdits";
import { activeId, confirm, isTableTab, useWorkspace, type TableTab } from "../../state/workspace";
import { ensureConsole, openTableTab, setTablePage } from "../editor/runActions";

type Intent = { tab: TableTab; target: ImportTarget; file: string; valid: () => boolean };
type Ready = { phase: "ready"; intent: Intent; source: ImportSourceId; header: boolean; preview: ImportPreview; mapping: (string | null)[] };
type Prompt =
  | { phase: "preparing"; intent: Intent; source: ImportSourceId | null; header: boolean }
  | Ready
  | (Omit<Ready, "phase"> & { phase: "submitting" })
  | { phase: "failed"; intent: Intent; source: ImportSourceId | null; message: string };

export const useImport = create<{ prompt: Prompt | null }>(() => ({ prompt: null }));
let sequence = 0;

export function dismissImport(): Promise<void> {
  sequence++;
  const { prompt } = useImport.getState();
  useImport.setState({ prompt: null });
  return prompt?.source ? commands.tableImportDiscard(prompt.intent.tab.id, prompt.source).then(() => {}) : Promise.resolve();
}

export async function cancelImport(): Promise<void> {
  const { prompt } = useImport.getState();
  await dismissImport();
  if (prompt?.phase !== "submitting") return;
  const snapshot = await commands.appBoot();
  const running = snapshot.consoles.find((console) => console.tabId === prompt.intent.tab.id)?.import;
  if (running?.source === prompt.source) await commands.consoleCancel(prompt.intent.tab.id);
}

function current(prompt: Prompt): boolean {
  return useImport.getState().prompt === prompt && prompt.intent.valid();
}

async function previewSource(prompt: Extract<Prompt, { phase: "preparing" }> & { source: ImportSourceId }): Promise<void> {
  const result = await commands.tableImportPreview(prompt.intent.tab.id, prompt.source, prompt.header);
  if (!current(prompt)) return;
  useImport.setState({
    prompt:
      result.status === "ok"
        ? { ...prompt, phase: "ready", preview: result.data, mapping: result.data.mapping }
        : { phase: "failed", intent: prompt.intent, source: prompt.source, message: importErrorText(result.error) },
  });
}

export async function beginImport(target: ImportTarget): Promise<void> {
  dismissImport();
  const attempt = sequence;
  const tabId = await openTableTab({ db: target.db, schema: target.schema, relation: target.table });
  if (!tabId || sequence !== attempt) return;
  const tab = useWorkspace.getState().tabs[tabId];
  if (!isTableTab(tab)) return;
  const config = useWorkspace.getState().configs[target.db.connectionId];
  const valid = () => {
    const workspace = useWorkspace.getState();
    const now = workspace.tabs[tabId];
    return (
      sequence === attempt &&
      activeId(workspace) === tabId &&
      workspace.screen.kind === "workspace" &&
      isTableTab(now) &&
      now.console === tab.console &&
      now.table.schema === target.schema &&
      now.table.table === target.table &&
      workspace.configs[target.db.connectionId] === config &&
      pendingCount(useTableEdits.getState().byTab[tabId] ?? NO_EDITS) === 0
    );
  };
  if (pendingCount(useTableEdits.getState().byTab[tabId] ?? NO_EDITS) > 0) {
    useWorkspace.getState().notify("Apply or revert the staged edits before importing CSV.");
    return;
  }
  if (!(await ensureConsole(tab)) || !valid()) return;
  const file = await open({ multiple: false, directory: false, filters: [{ name: "CSV", extensions: ["csv"] }] });
  if (!file || typeof file !== "string" || !valid()) return;
  const intent = { tab, target, file, valid };
  const preparing: Prompt = { phase: "preparing", intent, source: null, header: true };
  useImport.setState({ prompt: preparing });
  const result = await commands.tableImportPrepare(tabId, target, file);
  if (!current(preparing)) {
    if (result.status === "ok") await commands.tableImportDiscard(tabId, result.data);
    return;
  }
  if (result.status === "error") {
    useImport.setState({ prompt: { phase: "failed", intent, source: null, message: importErrorText(result.error) } });
    return;
  }
  const waiting = { ...preparing, source: result.data };
  useImport.setState({ prompt: waiting });
  await previewSource(waiting);
}

export function changeHeader(header: boolean): void {
  const { prompt } = useImport.getState();
  if (prompt?.phase !== "ready") return;
  const waiting = { phase: "preparing", intent: prompt.intent, source: prompt.source, header } satisfies Prompt;
  useImport.setState({ prompt: waiting });
  void previewSource(waiting);
}

export function changeMapping(mapping: (string | null)[]): void {
  const { prompt } = useImport.getState();
  if (prompt?.phase === "ready") useImport.setState({ prompt: { ...prompt, mapping } });
}

export async function submitImport(): Promise<void> {
  const { prompt } = useImport.getState();
  if (prompt?.phase !== "ready" || !current(prompt)) return;
  const submitting = { ...prompt, phase: "submitting" } satisfies Prompt;
  useImport.setState({ prompt: submitting });
  const { tab } = prompt.intent;
  const request = { source: prompt.source, header: prompt.header, mapping: prompt.mapping };
  let result = await commands.tableImport(tab.id, request, null);
  while (result.status === "error" && result.error.kind === "ConfirmWrites") {
    if (!current(submitting)) return;
    const asked = result.error.confirmation;
    const accepted = await confirm({
      title: `Import ${prompt.preview.rows.toLocaleString()} rows into ${prompt.intent.target.table} on Production?`,
      body: `${asked.connection} / ${asked.database}. This imports the prepared CSV with the mapping shown. ${asked.holds.count > 0 ? "Confirming also covers earlier writes held by this tab." : ""}`,
      statements: [...asked.holds.statements, ...asked.sends.statements],
      confirmLabel: "Import",
    });
    if (!current(submitting)) return;
    if (!accepted) {
      useImport.setState({ prompt });
      return;
    }
    result = await commands.tableImport(tab.id, request, asked.digest);
  }
  if (result.status === "error") {
    if (current(submitting))
      useImport.setState({ prompt: { phase: "failed", intent: prompt.intent, source: prompt.source, message: importErrorText(result.error) } });
    return;
  }
  const session = useLive.getState().consoles[tab.id]?.session;
  const refresh = prompt.intent.valid() && session?.kind === "Open" && session.backendPid === result.data.backendPid;
  if (useImport.getState().prompt === submitting) useImport.setState({ prompt: null });
  useWorkspace
    .getState()
    .notify(`${result.data.rows.toLocaleString()} rows imported${result.data.committed ? "." : "; pending commit. Rollback discards them."}`);
  if (refresh) setTablePage(tab.id, tab.table.page);
}

useWorkspace.subscribe(() => {
  const { prompt } = useImport.getState();
  if (prompt && (!prompt.intent.valid() || (prompt.phase !== "submitting" && useWorkspace.getState().overlay.kind !== "none"))) dismissImport();
});
useTableEdits.subscribe(() => {
  const { prompt } = useImport.getState();
  if (prompt && !prompt.intent.valid()) dismissImport();
});
