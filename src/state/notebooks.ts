import type { DatabaseRef, NotebookId, TabId } from "../ipc/bindings";
import { sameDb } from "./dbKey";

/** A database's notebook. Every cell in it is on `db`, and it is never empty. */
export type Notebook = { id: NotebookId; title: string; db: DatabaseRef | null; cells: TabId[]; active: TabId | null };

export type Notebooks = Record<NotebookId, Notebook>;

/** What the workspace shows: a notebook, or the start commands on a database that has none. `db: null` is no database. */
export type Shown = { kind: "notebook"; id: NotebookId } | { kind: "start"; db: DatabaseRef | null };

export const NO_DATABASE: Shown = { kind: "start", db: null };

export function notebookTitle(db: DatabaseRef | null): string {
  return db?.database ?? "No connection";
}

/** The notebook choosing `db` shows. A database has one notebook for now, so the first is the one. */
export function notebookFor(notebooks: Notebooks, db: DatabaseRef | null): Notebook | undefined {
  return Object.values(notebooks).find((n) => sameDb(n.db, db));
}

export function notebookOf(notebooks: Notebooks, cell: TabId): Notebook | undefined {
  return Object.values(notebooks).find((n) => n.cells.includes(cell));
}

export function newNotebook(db: DatabaseRef | null): Notebook {
  return { id: crypto.randomUUID(), title: notebookTitle(db), db, cells: [], active: null };
}

/**
 * Puts every cell in exactly one notebook on the cell's own database. A notebook keeps its cells that are still on its
 * database, in order. Every other cell, `order` first, joins the end of the notebook for its database, or a new one. A
 * notebook that loses its active cell takes the active cell that moved in, else its first. An empty notebook is dropped.
 */
export function settle(notebooks: Iterable<Notebook>, dbOf: (cell: TabId) => DatabaseRef | null | undefined, order: Iterable<TabId> = []): Notebooks {
  const placed = new Set<TabId>();
  const wasActive = new Set<TabId>();
  const kept = [...notebooks].map((n) => {
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
    let home = kept.find((k) => sameDb(k.notebook.db, db));
    if (!home) {
      home = { notebook: newNotebook(db), cells: [], movedIn: [] };
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
 * cell gone, the same notebook stays shown, or the start commands on its database when it is gone too.
 */
export function followActive(shown: Shown, before: Notebooks, after: Notebooks): { notebooks: Notebooks; shown: Shown } {
  if (shown.kind === "start") return { notebooks: after, shown };
  const old = before[shown.id];
  const holder = old?.active ? notebookOf(after, old.active) : undefined;
  if (holder && old?.active) {
    const notebooks = holder.active === old.active ? after : { ...after, [holder.id]: { ...holder, active: old.active } };
    return { notebooks, shown: { kind: "notebook", id: holder.id } };
  }
  return { notebooks: after, shown: after[shown.id] ? shown : { kind: "start", db: old?.db ?? null } };
}
