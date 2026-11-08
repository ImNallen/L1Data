import type { Text } from "@codemirror/state";
import { commands, type TabId } from "../../ipc/bindings";
import { disconnectErrorText } from "../../ipc/errors";
import { useLive } from "../../state/live";
import { pendingCount, useTableEdits } from "../../state/tableEdits";
import { ask, confirm, useWorkspace, type FileHealth, type Question, type Tab } from "../../state/workspace";
import { docOf, isDirty } from "./editorStates";
import { saveTab } from "./fileActions";
import { forgetTab, plural } from "./runActions";

const ws = () => useWorkspace.getState();
const exists = (id: TabId) => ws().tabs[id] !== undefined;

/**
 * Asks about each kind of work closing the tab would lose, and closes it only if every answer lets it. A question
 * whose answer loses nothing comes before one that destroys something: staged edits are dropped only at `forgetTab`,
 * and the rollback happens only in the last `consoleClose`, so a Cancel at any step loses nothing. `rollBack` is for a
 * caller that already said open transactions roll back, so it does not ask again.
 */
export async function closeTab(tabId: TabId, { rollBack = false } = {}): Promise<void> {
  const dropped = await keepText(tabId, null);
  if (dropped === false || !exists(tabId)) return;
  if (!(await dropEdits(tabId)) || !exists(tabId)) return;
  if (!(await closeConsole(tabId, rollBack)) || !exists(tabId)) return;
  // Text typed while an earlier step waited is not covered by any answer yet.
  if ((await keepText(tabId, dropped)) === false || !exists(tabId)) return;
  forgetTab(tabId);
}

/**
 * Resolves `false` to keep the tab, or the text the user chose not to save. A save writes the text the tab held when
 * it started, so text typed during it asks again.
 */
async function keepText(id: TabId, dropped: Text | null): Promise<Text | null | false> {
  for (;;) {
    const tab = ws().tabs[id];
    const doc = docOf(id) ?? null;
    if (!tab?.file || !isDirty(id) || doc === dropped) return dropped;
    const choice = await ask(saveQuestion(tab));
    if (choice === "discard") return doc;
    if (choice !== "save" || !(await saveTab(id))) return false;
  }
}

const SAVE_QUESTION_BODY: Record<FileHealth, string> = {
  ok: "Your changes are lost if you don't save them.",
  missing: "The file was moved or deleted. Saving writes it again where it was.",
  changed: "The file changed on disk. Don't Save keeps the file as it is now and drops this tab's text.",
  unreadable: "L1 Data couldn't read the file again. Don't Save drops this tab's text.",
};

function saveQuestion(tab: Tab): Question<"save" | "discard"> {
  return {
    title: `Save changes to ${tab.title}?`,
    body: SAVE_QUESTION_BODY[tab.file?.health ?? "ok"],
    choices: [
      { id: "discard", label: "Don't Save", variant: "ghost" },
      { id: "save", label: "Save", variant: "primary" },
    ],
  };
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
