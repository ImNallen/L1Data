import type { CellMeta, CellTable, ConnectionConfig, FileBinding, FileReadError, MetaPair, NotebookCell, NotebookOpened, NotebookText } from "../ipc/bindings";
import { notebookTitle, type Notebook, type SavedCell } from "./notebooks";
import type { Tab } from "./workspace";

const UNTITLED = /^Untitled-\d+\.sql$/;

/** A title a new cell or a table cell gets anyway is not worth writing to the file. */
export function recordedTitle(title: string | null, table: CellTable | null): string | null {
  return title === null || UNTITLED.test(title) || title === table?.table ? null : title;
}

/** What a save writes about a cell besides its text. `unknown` is the file's keys this build does not read. */
export function cellMeta(tab: Tab, unknown: MetaPair[] = []): CellMeta {
  const table = tab.table && { schema: tab.table.schema, table: tab.table.table };
  return { title: recordedTitle(tab.title, table), schema: tab.console.schema ?? null, table, manual: tab.console.mode === "Manual", unknown };
}

/** Keys this build does not read never change here, so they are left out. */
export function metaKey(meta: CellMeta): string {
  return JSON.stringify([recordedTitle(meta.title, meta.table), meta.schema, meta.table?.schema ?? null, meta.table?.table ?? null, meta.manual]);
}

export function savedCells(cells: readonly NotebookCell[]): SavedCell[] {
  return cells.map((c) => ({ text: c.text, meta: metaKey(c.meta) }));
}

function sameCells(a: readonly SavedCell[], b: readonly SavedCell[]): boolean {
  return a.length === b.length && a.every((c, i) => c.text === b[i]?.text && c.meta === b[i]?.meta);
}

/** The header names the connection and database by name only, so the file carries no credentials. */
export function notebookText(notebook: Notebook, cells: readonly NotebookCell[], config: ConnectionConfig | undefined): NotebookText {
  return {
    plain: notebook.file?.plain ?? false,
    header: {
      connection: config?.name ?? null,
      kind: config?.databaseKind ?? null,
      database: notebook.db?.database ?? null,
      unknown: notebook.file?.unknown.header ?? [],
    },
    cells: [...cells],
  };
}

/** `~` for the home folder at the start of the path. */
export function displayPath(path: string, home: string | null): string {
  if (!home || !path.startsWith(home) || !/^[\\/]/.test(path.slice(home.length))) return path;
  return `~${path.slice(home.length)}`;
}

export function fileName(path: string): string {
  return path.split(/[\\/]/).at(-1) ?? path;
}

export function fileStem(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(0, dot) : name;
}

/** The title a notebook gets without a rename: its database's name, numbered, or its file's name. */
export function hasDefaultTitle(notebook: Notebook): boolean {
  const name = notebookTitle(notebook.db);
  const numbered = notebook.title.startsWith(`${name} `) && /^\d+$/.test(notebook.title.slice(name.length + 1));
  return notebook.title === name || numbered || (notebook.file !== null && notebook.title === fileStem(fileName(notebook.file.binding.path)));
}

export type NotebookRead = { status: "ok"; data: NotebookOpened } | { status: "error"; error: FileReadError };

export type NotebookVerdict =
  | { kind: "adopt"; file: NotebookOpened }
  | { kind: "same"; file: NotebookOpened }
  | { kind: "reload"; file: NotebookOpened }
  | { kind: "diverged" }
  | { kind: "missing" }
  | { kind: "unreadable" };

/**
 * A restored notebook's fresh read of its file. The disk holds the cells, so adopt its binding, clean; the stamp
 * matches, so the disk is the baseline; it changed and the notebook was clean, so reload; it changed and the notebook
 * was not, so keep the cells and ask at the next save; missing or unreadable, so the baseline is unknown.
 */
export function judgeNotebook(binding: FileBinding, cells: readonly SavedCell[], dirty: boolean, read: NotebookRead): NotebookVerdict {
  if (read.status === "error") return read.error.kind === "NotFound" ? { kind: "missing" } : { kind: "unreadable" };
  const file = read.data;
  if (sameCells(savedCells(file.notebook.cells), cells)) return { kind: "adopt", file };
  if (file.binding.stamp === binding.stamp) return { kind: "same", file };
  return dirty ? { kind: "diverged" } : { kind: "reload", file };
}
