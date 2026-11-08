import type { NotebookId, TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import type { Notebook } from "../../state/notebooks";
import { hasUncommittedWork } from "../../state/status";
import { confirm, useWorkspace } from "../../state/workspace";
import { closeTab } from "../editor/closeTab";
import { textOf } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";

const ws = () => useWorkspace.getState();

export function openNotebookSwitcher(): void {
  ws().closeSettings();
  ws().open({ kind: "notebooks" });
}

/** Starts an empty notebook on the database shown now, and shows it. */
export function startNotebook(): TabId {
  return openSqlTab({ newNotebook: true });
}

export function holdsTransaction(cells: readonly TabId[]): boolean {
  const consoles = useLive.getState().consoles;
  return cells.some((id) => hasUncommittedWork(consoles[id]));
}

/** Nothing to lose: every cell is an untouched query cell with no transaction. */
function isBlank(notebook: Notebook): boolean {
  const { tabs } = ws();
  return !holdsTransaction(notebook.cells) && notebook.cells.every((id) => !tabs[id]?.table && !tabs[id]?.file && textOf(id).trim() === "");
}

/**
 * Closes `cells` in order, rolling back their transactions without asking. A cell's own questions still ask, and a
 * Cancel keeps it and every cell after it. Resolves `true` when every cell closed.
 */
export async function closeCells(cells: readonly TabId[]): Promise<boolean> {
  for (const id of cells) {
    await closeTab(id, { rollBack: true });
    if (ws().tabs[id]) return false;
  }
  return true;
}

/** Asks first unless the notebook is blank. Its last cell closing removes it. */
export async function deleteNotebook(id: NotebookId): Promise<void> {
  const notebook = ws().notebooks[id];
  if (!notebook) return;
  if (!isBlank(notebook)) {
    const n = notebook.cells.length;
    const ok = await confirm({
      title: `Delete ${notebook.title}?`,
      body: `${n === 1 ? "Its 1 cell closes." : `Its ${n} cells close.`}${holdsTransaction(notebook.cells) ? " Any open transactions are rolled back." : ""}`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
  }
  await closeCells(ws().notebooks[id]?.cells ?? []);
}
