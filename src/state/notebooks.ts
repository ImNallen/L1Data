import type { DatabaseRef, FileBinding, NotebookId, TabId, UnknownKeys } from "../ipc/bindings";
import { sameDb } from "./dbKey";

/**
 * What the last read or write found about a notebook's file. Not saved: boot reads every file again.
 * `changed`: the file changed on disk while the notebook held changes, so the next save asks before overwriting.
 * `unreadable`: the file is there, but reading it failed.
 */
export type FileHealth = "ok" | "missing" | "changed" | "unreadable";

/** One cell as the file held it: its text, and `metaKey` of what the file records about it. */
export type SavedCell = { text: string; meta: string };

export type NotebookFile = {
  binding: FileBinding;
  health: FileHealth;
  /** The file holds plain SQL. A save keeps it plain while the notebook is one cell with nothing else to record. */
  plain: boolean;
  unknown: UnknownKeys;
  /** The cells as the file held them at the last open, save, or read. `null` is unknown, which reads unsaved. */
  saved: SavedCell[] | null;
};

/**
 * One of a database's notebooks. Every cell in it is on `db`, and it is never empty. `shownAtMs` is when it was last
 * shown: choosing its database shows the most recent one. A notebook with a `file` is that file: saving writes every
 * cell to it.
 */
export type Notebook = {
  id: NotebookId;
  title: string;
  db: DatabaseRef | null;
  cells: TabId[];
  active: TabId | null;
  shownAtMs: number;
  file: NotebookFile | null;
};

export type Notebooks = Record<NotebookId, Notebook>;

/** What the workspace shows: a notebook, or the start commands on a database that has none. `db: null` is no database. */
export type Shown = { kind: "notebook"; id: NotebookId } | { kind: "start"; db: DatabaseRef | null };

export const NO_DATABASE: Shown = { kind: "start", db: null };

export function notebookTitle(db: DatabaseRef | null): string {
  return db?.database ?? "No connection";
}

/** The notebook choosing `db` shows: the one on `db` shown most recently, the first of a tie. */
export function notebookFor(notebooks: Notebooks | Notebook[], db: DatabaseRef | null): Notebook | undefined {
  let found: Notebook | undefined;
  for (const n of Array.isArray(notebooks) ? notebooks : Object.values(notebooks)) {
    if (sameDb(n.db, db) && (!found || n.shownAtMs > found.shownAtMs)) found = n;
  }
  return found;
}

export function notebookOf(notebooks: Notebooks, cell: TabId): Notebook | undefined {
  return Object.values(notebooks).find((n) => n.cells.includes(cell));
}

/** The cell `step` places before (-1) or after (1) `cell` in `cells`, if there is one. */
export function adjacentCell(cells: readonly TabId[], cell: TabId, step: -1 | 1): TabId | undefined {
  const at = cells.indexOf(cell);
  return at === -1 ? undefined : cells[at + step];
}

/** The database's name for its first notebook, then the lowest free "<name> 2", "<name> 3", and so on. */
export function newNotebook(existing: Iterable<Notebook>, db: DatabaseRef | null): Notebook {
  const name = notebookTitle(db);
  const taken = new Set([...existing].filter((n) => sameDb(n.db, db)).map((n) => n.title));
  let title = name;
  for (let i = 2; taken.has(title); i++) title = `${name} ${i}`;
  return { id: crypto.randomUUID(), title, db, cells: [], active: null, shownAtMs: 0, file: null };
}

/** Marks `id` shown later than every other notebook, at the current time unless the clock has not moved past them. */
export function stampShown(notebooks: Notebooks, id: NotebookId, now = Date.now()): Notebooks {
  const notebook = notebooks[id];
  if (!notebook) return notebooks;
  const latest = Math.max(...Object.values(notebooks).map((n) => (n === notebook ? -Infinity : n.shownAtMs)));
  if (notebook.shownAtMs > latest) return notebooks;
  return { ...notebooks, [id]: { ...notebook, shownAtMs: Math.max(now, latest + 1) } };
}

/**
 * A notebook whose cells all moved to one other database moves with them, keeping its identity, title and file. A
 * title that only named the old database names the new one.
 */
function followCells(dbOf: (cell: TabId) => DatabaseRef | null | undefined): (n: Notebook) => Notebook {
  return (n) => {
    const dbs = n.cells.map(dbOf).filter((db) => db !== undefined);
    const [first] = dbs;
    if (first === undefined || dbs.length !== n.cells.length || sameDb(first, n.db) || !dbs.every((db) => sameDb(db, first))) return n;
    const title = n.file === null && n.title === notebookTitle(n.db) ? notebookTitle(first) : n.title;
    return { ...n, db: first, title };
  };
}

/**
 * Puts every cell in exactly one notebook on the cell's own database. A notebook keeps its cells that are still on its
 * database, in order. Every other cell, `order` first, joins the end of the most recent notebook on its database, or a
 * new one. A notebook that loses its active cell takes the active cell that moved in, else its first. An empty notebook
 * is dropped.
 */
export function settle(notebooks: Iterable<Notebook>, dbOf: (cell: TabId) => DatabaseRef | null | undefined, order: Iterable<TabId> = []): Notebooks {
  const placed = new Set<TabId>();
  const wasActive = new Set<TabId>();
  const kept = [...notebooks].map(followCells(dbOf)).map((n) => {
    if (n.active) wasActive.add(n.active);
    const cells = n.cells.filter((id) => {
      const db = dbOf(id);
      if (db === undefined || placed.has(id) || !sameDb(db, n.db)) return false;
      placed.add(id);
      return true;
    });
    return { notebook: n, cells, movedIn: [] as TabId[] };
  });
  const loose = [...order, ...kept.flatMap((k) => k.notebook.cells)];
  for (const id of loose) {
    const db = dbOf(id);
    if (db === undefined || placed.has(id)) continue;
    placed.add(id);
    const recent = notebookFor(
      kept.map((k) => k.notebook),
      db,
    );
    let home = kept.find((k) => k.notebook === recent);
    if (!home) {
      home = {
        notebook: newNotebook(
          kept.map((k) => k.notebook),
          db,
        ),
        cells: [],
        movedIn: [],
      };
      kept.push(home);
    }
    home.cells.push(id);
    home.movedIn.push(id);
  }
  const settled: Notebooks = {};
  for (const { notebook, cells, movedIn } of kept) {
    if (cells.length === 0) continue;
    const active = notebook.active && cells.includes(notebook.active) ? notebook.active : (movedIn.find((id) => wasActive.has(id)) ?? cells[0] ?? null);
    const same = active === notebook.active && cells.length === notebook.cells.length && cells.every((id, i) => notebook.cells[i] === id);
    settled[notebook.id] = same ? notebook : { ...notebook, cells, active };
  }
  return settled;
}

/**
 * Keeps the active cell in view after `settle` moved cells: the notebook now holding it is shown with it active. With the
 * cell gone, the same notebook stays shown, or else the most recent other notebook on its database, or the start
 * commands on it.
 */
export function followActive(shown: Shown, before: Notebooks, after: Notebooks): { notebooks: Notebooks; shown: Shown } {
  if (shown.kind === "start") return { notebooks: after, shown };
  const old = before[shown.id];
  const holder = old?.active ? notebookOf(after, old.active) : undefined;
  if (holder && old?.active) {
    const notebooks = holder.active === old.active ? after : { ...after, [holder.id]: { ...holder, active: old.active } };
    return showNotebook(notebooks, holder.id);
  }
  if (after[shown.id]) return { notebooks: after, shown };
  return showDatabase(after, old?.db ?? null);
}

/** Shows `id` and marks it the most recent. */
export function showNotebook(notebooks: Notebooks, id: NotebookId): { notebooks: Notebooks; shown: Shown } {
  return { notebooks: stampShown(notebooks, id), shown: { kind: "notebook", id } };
}

/** Shows the most recent notebook on `db`, or the start commands on it when it has none. */
export function showDatabase(notebooks: Notebooks, db: DatabaseRef | null): { notebooks: Notebooks; shown: Shown } {
  const notebook = notebookFor(notebooks, db);
  return notebook ? showNotebook(notebooks, notebook.id) : { notebooks, shown: { kind: "start", db } };
}
