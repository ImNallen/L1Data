import type { ConnectionConfig, ConnectionId, ConsoleState, DatabaseRef, TabId } from "../../ipc/bindings";
import { sameDb } from "../../state/dbKey";
import { displayPath } from "../../state/notebookFile";
import type { Notebook, Notebooks, Shown } from "../../state/notebooks";
import { notebookActivity } from "../../state/status";

export type SwitcherRow =
  | { kind: "notebook"; key: string; notebook: Notebook; open: boolean; context: string; action: string }
  | { kind: "new"; key: "new"; db: DatabaseRef; context: string };

export type SwitcherGroup = { label: "Current database" | "Other databases" | "Actions"; rows: SwitcherRow[] };

export type SwitcherSource = {
  notebooks: Notebooks;
  shown: Shown;
  configs: Record<ConnectionId, ConnectionConfig>;
  consoles: Record<TabId, ConsoleState>;
  homeDir: string | null;
};

const ACTIVITY = { running: "Running", transaction: "Open transaction" } as const;

/**
 * The shown database's notebooks, then every other notebook, each most recently shown first, then the actions. A query
 * keeps the notebooks whose title or file path holds it. `total` counts every notebook.
 */
export function switcherGroups({ notebooks, shown, configs, consoles, homeDir }: SwitcherSource, query: string): { groups: SwitcherGroup[]; total: number } {
  const db = shown.kind === "start" ? shown.db : (notebooks[shown.id]?.db ?? null);
  const openId = shown.kind === "notebook" ? shown.id : null;
  const needle = query.trim().toLowerCase();
  const all = Object.values(notebooks).sort((a, b) => b.shownAtMs - a.shownAtMs);
  const path = (n: Notebook) => (n.file ? displayPath(n.file.binding.path, homeDir) : null);
  const row = (n: Notebook, current: boolean): SwitcherRow => {
    const activity = notebookActivity(consoles, n.cells);
    const where = n.db ? `${n.db.database} · ${configs[n.db.connectionId]?.name ?? "No connection"}` : "No connection";
    const parts = [path(n) ?? "Not saved to a file", ...(current ? [] : [where])];
    if (activity) parts.push(ACTIVITY[activity]);
    return {
      kind: "notebook",
      key: n.id,
      notebook: n,
      open: n.id === openId,
      context: parts.join(" · "),
      action: n.id === openId ? "Open now" : current ? "Switch" : "Switch database",
    };
  };
  const matching = all.filter((n) => n.title.toLowerCase().includes(needle) || (n.file?.binding.path.toLowerCase().includes(needle) ?? false));
  const groups: SwitcherGroup[] = [
    { label: "Current database", rows: matching.filter((n) => sameDb(n.db, db)).map((n) => row(n, true)) },
    { label: "Other databases", rows: matching.filter((n) => !sameDb(n.db, db)).map((n) => row(n, false)) },
    { label: "Actions", rows: db ? [{ kind: "new", key: "new", db, context: `Start an empty notebook on ${db.database}` }] : [] },
  ];
  return { groups: groups.filter((g) => g.rows.length > 0), total: all.length };
}
