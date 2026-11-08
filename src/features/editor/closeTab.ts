import { commands, type TabId } from "../../ipc/bindings";
import { disconnectErrorText } from "../../ipc/errors";
import { useLive } from "../../state/live";
import { notebookOf } from "../../state/notebooks";
import { pendingCount, useTableEdits } from "../../state/tableEdits";
import { confirm, useWorkspace } from "../../state/workspace";
import { keepNotebook, type Answered } from "../notebooks/notebookFiles";
import { forgetTab, plural } from "./runActions";

const ws = () => useWorkspace.getState();
const exists = (id: TabId) => ws().tabs[id] !== undefined;

/** Closing a file notebook's other cells only changes the notebook; closing its last one closes the file. */
function lastFileCell(id: TabId) {
  const notebook = notebookOf(ws().notebooks, id);
  return notebook?.file && notebook.cells.length === 1 ? notebook.id : null;
}

/**
 * Asks about each kind of work closing the tab would lose, and closes it only if every answer lets it. A question
 * whose answer loses nothing comes before one that destroys something: staged edits are dropped only at `forgetTab`,
 * and the rollback happens only in the last `consoleClose`, so a Cancel at any step loses nothing. `rollBack` is for a
 * caller that already said open transactions roll back, and `fileAnswered` for one that already asked to save the
 * notebook's file, so neither asks again.
 */
export async function closeTab(tabId: TabId, { rollBack = false, fileAnswered = false } = {}): Promise<void> {
  const keep = (dropped: Answered | null) => {
    const notebook = fileAnswered ? null : lastFileCell(tabId);
    return notebook ? keepNotebook(notebook, dropped) : Promise.resolve(dropped);
  };
  const dropped = await keep(null);
  if (dropped === false || !exists(tabId)) return;
  if (!(await dropEdits(tabId)) || !exists(tabId)) return;
  if (!(await closeConsole(tabId, rollBack)) || !exists(tabId)) return;
  // A change made while an earlier step waited is not covered by any answer yet.
  if ((await keep(dropped)) === false || !exists(tabId)) return;
  forgetTab(tabId);
}

async function dropEdits(id: TabId): Promise<boolean> {
  const edits = useTableEdits.getState().byTab[id];
  if (!edits || pendingCount(edits) === 0) return true;
  return confirm({
    title: "Close tab with unsaved changes?",
    body: `Closing discards ${plural(pendingCount(edits), "change")} that haven't been submitted.`,
    confirmLabel: "Discard and close",
  });
}

async function closeConsole(id: TabId, rollBack: boolean): Promise<boolean> {
  if (!useLive.getState().consoles[id]) return true;
  const res = await commands.consoleClose(id, rollBack);
  if (res.status === "ok") return true;
  if (res.error.kind !== "OpenTransactions") {
    ws().notify(disconnectErrorText(res.error));
    return false;
  }
  const agreed = await confirm({
    title: "Close tab with an open transaction?",
    body: "Closing rolls back the uncommitted work in this tab.",
    confirmLabel: "Roll back and close",
  });
  if (!agreed || !exists(id)) return false;
  await commands.consoleClose(id, true);
  return true;
}
