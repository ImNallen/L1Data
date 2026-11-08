import type { NotebookId, TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import type { Notebook } from "../../state/notebooks";
import { hasUncommittedWork } from "../../state/status";
import { confirm, useWorkspace } from "../../state/workspace";
import { closeTab } from "../editor/closeTab";
import { textOf } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { notebookDirty } from "./notebookDirty";
import { keepNotebook } from "./notebookFiles";

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

/** Nothing to lose: no transaction, and either a saved file holds every cell or every cell is an untouched query cell. */
function isBlank(notebook: Notebook): boolean {
  const { tabs } = ws();
  if (holdsTransaction(notebook.cells)) return false;
  return notebook.file ? !notebookDirty(notebook.id) : notebook.cells.every((id) => !tabs[id]?.table && textOf(id).trim() === "");
}

/**
 * Closes `cells` in order, rolling back their transactions without asking. A cell's own questions still ask, and a
 * Cancel keeps it and every cell after it. Resolves `true` when every cell closed. The caller has already asked to save
 * any file notebook the cells belong to.
 */
export async function closeCells(cells: readonly TabId[]): Promise<boolean> {
  for (const id of cells) {
    await closeTab(id, { rollBack: true, fileAnswered: true });
    if (ws().tabs[id]) return false;
  }
  return true;
}

const ROLLED_BACK = "Any open transactions are rolled back.";

/**
 * An unsaved file notebook asks once to save, with Don't Save deleting it; any other asks first unless it is blank. Its
 * last cell closing removes it, and its file stays on disk.
 */
export async function deleteNotebook(id: NotebookId): Promise<void> {
  const notebook = ws().notebooks[id];
  if (!notebook) return;
  const transactions = holdsTransaction(notebook.cells) ? ROLLED_BACK : undefined;
  if (notebook.file && notebookDirty(id)) {
    if ((await keepNotebook(id, null, transactions)) === false) return;
  } else if (!isBlank(notebook)) {
    const n = notebook.cells.length;
    const ok = await confirm({
      title: `Delete ${notebook.title}?`,
      body: `${n === 1 ? "Its 1 cell closes." : `Its ${n} cells close.`}${transactions ? ` ${transactions}` : ""}`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
  }
  await closeCells(ws().notebooks[id]?.cells ?? []);
}
