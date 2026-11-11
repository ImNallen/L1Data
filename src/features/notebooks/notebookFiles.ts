import type { Text } from "@codemirror/state";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  commands,
  type ConnectionConfig,
  type ConnectionId,
  type ConnectionState,
  type DatabaseRef,
  type FileBinding,
  type NotebookCell,
  type NotebookHeader,
  type NotebookId,
  type NotebookOpened,
  type NotebookSaved,
  type SaveTarget,
  type UnknownKeys,
} from "../../ipc/bindings";
import { fileReadErrorText, fileWriteErrorText } from "../../ipc/errors";
import { databaseKindFor } from "../../state/databaseKind";
import { homeDb, resolveDb } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { cellMeta, fileStem, hasDefaultTitle, judgeNotebook, metaKey, notebookText, recordedTitle, savedCells } from "../../state/notebookFile";
import { newNotebook, type FileHealth, type Notebook, type SavedCell } from "../../state/notebooks";
import { ask, EMPTY_QUERY, nextUntitled, notebookWithFile, shownDb, useWorkspace, type Question, type Tab } from "../../state/workspace";
import { docOf, replaceDoc, seedEditor, textOf } from "../editor/editorStates";
import { flushWorkspaceSave } from "../editor/persistence";
import { forgetTab } from "../editor/runActions";
import { notebookDirty } from "./notebookDirty";

const ws = () => useWorkspace.getState();
const notify = (text: string) => ws().notify(text);

const SQL_FILTER = { name: "SQL", extensions: ["sql"] };

/** Does nothing while a dialog is up, since leaving settings would close it and so cancel a question. */
export async function openNotebookFiles(): Promise<void> {
  if (ws().overlay.kind !== "none") return;
  ws().closeSettings();
  for (const path of (await pickFiles()) ?? []) await openNotebookFile(path);
}

async function openNotebookFile(path: string): Promise<void> {
  const open = notebookWithFile(ws().notebooks, path);
  if (open) return ws().showNotebook(open.id);
  const res = await commands.notebookOpen(path);
  if (res.status === "error") return notify(fileReadErrorText(res.error, path));
  // The engine names a file by its canonical path, which a notebook opened through another spelling of it already has.
  const same = notebookWithFile(ws().notebooks, res.data.binding.path);
  if (same) return ws().showNotebook(same.id);
  const { configs } = ws();
  const { db, missing } = notebookDatabase(res.data.notebook.header, configs, shownDb(ws()), useLive.getState().connections);
  if (missing !== null)
    notify(`No connection named “${missing}”. ${db ? `Opened on ${db.database} · ${configs[db.connectionId]?.name}.` : "Opened with no connection."}`);
  const { notebook } = res.data;
  const tabs = cellTabs(notebook.cells, db);
  tabs.forEach((t, i) => seedEditor(t.id, notebook.cells[i]?.text ?? ""));
  ws().openNotebook(
    {
      ...newNotebook([], db),
      title: fileStem(res.data.name),
      cells: tabs.map((t) => t.id),
      active: tabs[0]?.id ?? null,
      file: { binding: res.data.binding, health: "ok", plain: notebook.plain, unknown: unknownKeys(res.data, tabs), saved: baseline(tabs, notebook.cells) },
    },
    tabs,
  );
}

/**
 * The header's connection when one here has its name and kind. Otherwise the shown database when the kind fits, which a
 * plain file always does, else the first connection of the kind. `missing` names a header connection not found here.
 */
export function notebookDatabase(
  header: NotebookHeader,
  configs: Record<ConnectionId, ConnectionConfig>,
  shown: DatabaseRef | null,
  connections: Record<ConnectionId, ConnectionState>,
): { db: DatabaseRef | null; missing: string | null } {
  const fits = (c: ConnectionConfig | undefined) => header.kind === null || c?.databaseKind === header.kind;
  const named = header.connection === null ? undefined : Object.values(configs).find((c) => c.name === header.connection && fits(c));
  if (named) {
    const db =
      header.database === null ? homeDb(named, connections[named.id]) : resolveDb({ connectionId: named.id, database: header.database }, connections[named.id]);
    return { db, missing: null };
  }
  const missing = header.connection;
  if (shown && fits(configs[shown.connectionId])) return { db: shown, missing };
  const first = header.kind === null ? undefined : Object.values(configs).find(fits);
  return { db: first ? homeDb(first, connections[first.id]) : null, missing };
}

/**
 * A cell the file does not title takes a new cell's title, or keeps the one `reuse` gave the cell at its place, as it keeps
 * that cell's collapse. A table cell browses only on a connection, and SQL Server cells commit automatically.
 */
function cellTabs(cells: readonly NotebookCell[], db: DatabaseRef | null, reuse: readonly (Tab | undefined)[] = []): Tab[] {
  const manualOk = databaseKindFor(db, ws().configs) !== "SqlServer";
  const tabs: Tab[] = [];
  cells.forEach(({ meta }, i) => {
    const table = db && meta.table ? { schema: meta.table.schema, table: meta.table.table, page: 0, query: EMPTY_QUERY } : null;
    const old = reuse[i];
    const untitled = old && recordedTitle(old.title, table) === null ? old.title : nextUntitled([...Object.values(ws().tabs), ...tabs]);
    tabs.push({
      id: old?.id ?? crypto.randomUUID(),
      title: meta.title ?? table?.table ?? untitled,
      console: { db, mode: meta.manual && manualOk ? "Manual" : "AutoCommit", ...(meta.schema && { schema: meta.schema }) },
      table,
      ...(old?.collapsed && { collapsed: true }),
    });
  });
  return tabs;
}

/** What the file holds, as the cells it made: anything a cell could not take reads as a change. */
function baseline(tabs: readonly Tab[], cells: readonly NotebookCell[]): SavedCell[] {
  return tabs.map((t, i) => ({ text: cells[i]?.text ?? "", meta: metaKey(cellMeta(t)) }));
}

function unknownKeys(opened: NotebookOpened, tabs: readonly Tab[]): UnknownKeys {
  const cells = Object.fromEntries(
    tabs.flatMap((t, i) => (opened.notebook.cells[i]?.meta.unknown.length ? [[t.id, opened.notebook.cells[i].meta.unknown]] : [])),
  );
  return { header: opened.notebook.header.unknown, cells };
}

const sameBinding = (id: NotebookId, binding: FileBinding | undefined) => {
  const notebook = ws().notebooks[id];
  return notebook !== undefined && notebook.file?.binding === binding;
};

export type SaveMode = "save" | "saveAs";

const saving = new Map<NotebookId, Promise<boolean>>();

/**
 * Resolves `true` once every cell is on disk and the notebook is bound to the file. Saves of one notebook run in order,
 * so a second ⌘S compares the disk against the stamp the first one wrote and never mistakes our own write for an
 * outside change.
 */
export function saveNotebook(id: NotebookId, mode: SaveMode = "save"): Promise<boolean> {
  const once = () => saveOnce(id, mode).catch((e: unknown) => (notify(String(e)), false));
  const run = (saving.get(id) ?? Promise.resolve(true)).then(once);
  saving.set(id, run);
  void run.then(() => {
    if (saving.get(id) === run) saving.delete(id);
  });
  return run;
}

export function savesInFlight(): Promise<void> {
  return Promise.all(saving.values()).then(() => undefined);
}

async function saveOnce(id: NotebookId, mode: SaveMode): Promise<boolean> {
  const notebook = ws().notebooks[id];
  if (!notebook) return false;
  const binding = notebook.file?.binding;
  if (mode === "save" && binding) return write(id, binding, { kind: "Bound", binding, overwrite: null });
  const path = await pickSavePath(binding?.path ?? defaultFileName(notebook.title));
  if (!path || !sameBinding(id, binding)) return false;
  const taken = Object.values(ws().notebooks).flatMap((n) => (n.id !== id && n.file ? [n.file.binding.path] : []));
  return write(id, binding, { kind: "New", path, format: binding?.format ?? null, taken });
}

function cellsOf(notebook: Notebook): NotebookCell[] {
  const { tabs } = ws();
  return notebook.cells.flatMap((id) => {
    const tab = tabs[id];
    return tab ? [{ text: textOf(id), meta: cellMeta(tab, notebook.file?.unknown.cells[id]) }] : [];
  });
}

/**
 * Sends the cells as they are when the write starts, and the same cells again after an overwrite question, so what lands
 * on disk is always what the notebook held. Stops before any write once the notebook is gone or bound elsewhere.
 */
async function write(id: NotebookId, binding: FileBinding | undefined, first: SaveTarget): Promise<boolean> {
  const notebook = ws().notebooks[id];
  if (!notebook) return false;
  const cells = cellsOf(notebook);
  const text = notebookText(notebook, cells, notebook.db ? ws().configs[notebook.db.connectionId] : undefined);
  // The stored buffers are then never older than the file, so a crash after the write restores cells that match it.
  await flushWorkspaceSave();
  let target = first;
  for (;;) {
    if (!sameBinding(id, binding)) return false;
    const res = await commands.notebookSave(target, text);
    if (res.status === "ok") {
      adopt(id, res.data, cells, target.kind === "New");
      return true;
    }
    if (res.error.kind !== "Changed" || target.kind !== "Bound") {
      notify(fileWriteErrorText(res.error, target));
      return false;
    }
    if (!sameBinding(id, binding)) return false;
    // The disk holds cells this notebook never read, so unless the overwrite goes through the notebook is not clean.
    ws().patchNotebookFile(id, { saved: null, health: "changed" });
    if ((await ask(overwriteQuestion(ws().notebooks[id]?.title ?? ""))) !== "overwrite") return false;
    target = { ...target, overwrite: res.error.disk };
  }
}

/** A save to a new file renames a notebook that still has the title it was given, so it reads as the file. */
function adopt(id: NotebookId, saved: NotebookSaved, cells: NotebookCell[], newFile: boolean): void {
  const notebook = ws().notebooks[id];
  if (!notebook) return;
  const title = newFile && hasDefaultTitle(notebook) ? fileStem(saved.name) : undefined;
  const unknown = notebook.file?.unknown ?? { header: [], cells: {} };
  ws().setNotebookFile(id, { binding: saved.binding, health: "ok", plain: saved.plain, unknown, saved: savedCells(cells) }, title);
}

function overwriteQuestion(title: string): Question<"overwrite"> {
  return {
    title: `${title} changed on disk`,
    body: "Another program changed the file after this notebook last opened or saved it. Overwrite it with this notebook?",
    choices: [{ id: "overwrite", label: "Overwrite", variant: "danger" }],
  };
}

export function defaultFileName(title: string): string {
  const name = title.replace(/[/\\:]/g, "-");
  return /\.\w{1,10}$/.test(name) ? name : `${name}.sql`;
}

/** What a close question's answer covers: the cells, their docs, and what the file records about each. */
export type Answered = { cells: string; docs: (Text | undefined)[] };

function answeredOf(notebook: Notebook): Answered {
  const { tabs } = ws();
  return { cells: JSON.stringify(notebook.cells.map((id) => [id, tabs[id] ? metaKey(cellMeta(tabs[id])) : null])), docs: notebook.cells.map(docOf) };
}

const sameAnswer = (a: Answered, b: Answered) => a.cells === b.cells && a.docs.length === b.docs.length && a.docs.every((d, i) => d === b.docs[i]);

/**
 * Asks once to save an unsaved file notebook that is about to go. Resolves `false` to keep it, else what the user chose
 * not to save, or `dropped` when nothing changed since that answer. A save writes the cells as they were when it
 * started, so a change during it asks again.
 */
export async function keepNotebook(id: NotebookId, dropped: Answered | null = null, note?: string): Promise<Answered | null | false> {
  for (;;) {
    const notebook = ws().notebooks[id];
    if (!notebook?.file || !notebookDirty(id)) return dropped;
    const now = answeredOf(notebook);
    if (dropped && sameAnswer(dropped, now)) return dropped;
    const choice = await ask(saveQuestion(notebook, note));
    if (choice === "discard") return now;
    if (choice !== "save" || !(await saveNotebook(id))) return false;
  }
}

const SAVE_QUESTION_BODY: Record<FileHealth, string> = {
  ok: "Your changes are lost if you don't save them.",
  missing: "The file was moved or deleted. Saving writes it again where it was.",
  changed: "The file changed on disk. Don't Save keeps the file as it is now and drops this notebook's changes.",
  unreadable: "L1 Data couldn't read the file again. Don't Save drops this notebook's changes.",
};

function saveQuestion(notebook: Notebook, note: string | undefined): Question<"save" | "discard"> {
  return {
    title: `Save changes to ${notebook.title}?`,
    body: SAVE_QUESTION_BODY[notebook.file?.health ?? "ok"],
    note,
    choices: [
      { id: "discard", label: "Don't Save", variant: "ghost" },
      { id: "save", label: "Save", variant: "primary" },
    ],
  };
}

/** Reads every notebook's file once, at boot. The app does not watch files while it runs. */
export async function reconcileNotebookFiles(): Promise<void> {
  const reads = Object.values(ws().notebooks).flatMap((n) => (n.file ? [reconcile(n.id, n.file.binding)] : []));
  const reloaded = (await Promise.all(reads)).filter((name) => name !== null);
  if (reloaded.length > 0) notify(reloaded.length === 1 ? `Reloaded ${reloaded[0]} from disk.` : `Reloaded ${reloaded.length} files from disk.`);
}

function currentCells(notebook: Notebook): SavedCell[] {
  return savedCells(cellsOf(notebook));
}

async function reconcile(id: NotebookId, binding: FileBinding): Promise<string | null> {
  const before = ws().notebooks[id];
  if (!before) return null;
  const started = answeredOf(before);
  const read = await commands.notebookOpen(binding.path);
  const notebook = ws().notebooks[id];
  // A verdict applies only to the cells its read started with.
  if (!notebook?.file || !sameBinding(id, binding) || !sameAnswer(started, answeredOf(notebook))) return null;
  const verdict = judgeNotebook(binding, currentCells(notebook), notebookDirty(id), read);
  // The verdict compares known keys only, so a file kept in place still brings the keys another program changed.
  const kept = () => notebook.cells.flatMap((cell) => ws().tabs[cell] ?? []);
  switch (verdict.kind) {
    case "adopt":
      ws().setNotebookFile(id, {
        ...notebook.file,
        binding: verdict.file.binding,
        health: "ok",
        plain: verdict.file.notebook.plain,
        unknown: unknownKeys(verdict.file, kept()),
        saved: currentCells(notebook),
      });
      return null;
    case "same":
      ws().setNotebookFile(id, {
        ...notebook.file,
        health: "ok",
        plain: verdict.file.notebook.plain,
        unknown: unknownKeys(verdict.file, kept()),
        saved: savedCells(verdict.file.notebook.cells),
      });
      return null;
    case "reload":
      reload(notebook, verdict.file);
      return verdict.file.name;
    case "diverged":
      ws().patchNotebookFile(id, { health: "changed" });
      return null;
    case "missing":
    case "unreadable":
      ws().patchNotebookFile(id, { saved: null, health: verdict.kind });
      return null;
    default: {
      const unreachable: never = verdict;
      return unreachable;
    }
  }
}

/** Cells keep their place, and each kept cell's text changes as one undoable edit. */
function reload(notebook: Notebook, opened: NotebookOpened): void {
  const { cells } = opened.notebook;
  const kept = notebook.cells.map((id) => ws().tabs[id]);
  const tabs = cellTabs(cells, notebook.db, kept);
  tabs.forEach((t, i) => (i < notebook.cells.length ? replaceDoc(t.id, cells[i]?.text ?? "") : seedEditor(t.id, cells[i]?.text ?? "")));
  ws().replaceCells(notebook.id, tabs);
  for (const id of notebook.cells.slice(tabs.length)) forgetTab(id);
  ws().setNotebookFile(notebook.id, {
    binding: opened.binding,
    health: "ok",
    plain: opened.notebook.plain,
    unknown: unknownKeys(opened, tabs),
    saved: baseline(tabs, cells),
  });
}

async function pickFiles(): Promise<string[] | null> {
  try {
    return await openDialog({ multiple: true, directory: false, filters: [SQL_FILTER, { name: "All files", extensions: ["*"] }] });
  } catch {
    notify("Opening files needs the desktop app's file dialog.");
    return null;
  }
}

/** The OS dialog asks before replacing a file, so the path it returns is consent to replace it. */
async function pickSavePath(defaultPath: string): Promise<string | null> {
  try {
    return await saveDialog({ defaultPath, filters: [SQL_FILTER] });
  } catch {
    notify("Saving needs the desktop app's save dialog.");
    return null;
  }
}
