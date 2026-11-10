import { create } from "zustand";
import type {
  ConnectionConfig,
  ConnectionId,
  ConnectionState,
  DatabaseRef,
  Layout,
  NotebookId,
  NotebookSnapshot,
  ResultMeta,
  SavedQueryId,
  Settings,
  TabId,
  TableQuery,
  TabSnapshot,
  TxnMode,
  Workspace,
} from "../ipc/bindings";
import type { SettingsSectionId } from "../features/settings/settingsIndex";
import { homeDb, resolveDb, sameDb } from "./dbKey";
import { useLive } from "./live";
import { cellMeta, fileName, fileStem, metaKey } from "./notebookFile";
import {
  followActive,
  NO_DATABASE,
  newNotebook,
  notebookFor,
  notebookOf,
  settle,
  showDatabase,
  showNotebook,
  type Notebook,
  type NotebookFile,
  type Notebooks,
  type Shown,
} from "./notebooks";
import type { CellSource as Source } from "../ipc/bindings";
import type { ChartSpec } from "../features/results/chart";
import type { ResultDisplay, ResultPick } from "./status";

/** A tab's console target. A tab never names a connection without a database. */
export type ResultDisplayPref = { key: string; display: ResultDisplay; chart: ChartSpec | null };

export type ConsoleTarget = { db: DatabaseRef | null; mode: TxnMode; schema?: string | null };

/** The table a tab opened from the sidebar browses, in the tab's console database. */
export type TableBrowse = { schema: string; table: string; page: number; query: TableQuery; exactRowCount?: number };

/** One cell of a notebook. A file belongs to the notebook, not to a cell. */
export type Tab = { id: TabId; title: string; console: ConsoleTarget; table: TableBrowse | null };
/** A tab bound to a table. Its console always names a database. */
export type TableTab = Tab & { table: TableBrowse; console: ConsoleTarget & { db: DatabaseRef } };

export function isTableTab(tab: Tab | undefined): tab is TableTab {
  return tab !== undefined && tab.table !== null && tab.console.db !== null;
}

export const EMPTY_QUERY: TableQuery = { orderBy: "", filters: [], where: "" };

export function isFiltered(q: TableQuery): boolean {
  return q.filters.length > 0 || q.where.trim() !== "";
}

export type RelationRef = { db: DatabaseRef; schema: string; relation: string };

export type SavedEntry = { kind: "browse" } | { kind: "name"; id: SavedQueryId } | { kind: "existing"; id: SavedQueryId };

export type QueriesTab = "history" | "saved";
export type QueriesOverlay = { kind: "queries"; tab: "history" } | { kind: "queries"; tab: "saved"; entry: SavedEntry };

export type Overlay =
  | { kind: "none" }
  | QueriesOverlay
  | { kind: "navigation" }
  | { kind: "notebooks" }
  | { kind: "commands" }
  | { kind: "usage" }
  | { kind: "connection"; editing: ConnectionId | null }
  | { kind: "cell"; title: string; value: string | null }
  | { kind: "record"; meta: ResultMeta; row: number }
  | ({ kind: "ask"; seq: number; settle: (choice: string | null) => void } & Required<Question<string>>);

export type Choice<C extends string> = { id: C; label: string; variant: "primary" | "danger" | "ghost" };

/**
 * Every question also has a Cancel button. Cancel, Esc, the close button, and any overlay that replaces the
 * question all answer `null`. The dialog's test ids are `<testId>-dialog`, `<testId>-cancel`, and
 * `<testId>-<choice id>`.
 */
export type Question<C extends string> = {
  testId?: string;
  title: string;
  body: string;
  statements?: readonly string[];
  /** Plain rows under the body, each a label with a dim detail at the right. */
  rows?: readonly QuestionRow[];
  /** A dim line under the rows. */
  note?: string;
  choices: readonly Choice<C>[];
};

export type QuestionRow = { label: string; detail: string };

/** Command context; the workspace stays mounted beneath settings. `query` lists matches instead of `section`. */
export type Screen = { kind: "workspace" } | { kind: "settings"; section: SettingsSectionId; query: string };

export type WorkspaceData = {
  tabs: Record<TabId, Tab>;
  notebooks: Notebooks;
  shown: Shown;
  layout: Layout;
  configs: Record<ConnectionId, ConnectionConfig>;
  activeResult: Record<TabId, ResultPick>;
  /** Rows, plan, or chart for each result of the tab's latest run, keyed by `resultKey`. A new run starts from the default. */
  resultDisplay: Record<TabId, Record<string, ResultDisplayPref>>;
  /** The cells the tab's latest run inlined through `{{name}}`, with the text it used. Kept in memory, as results are. */
  runSources: Record<TabId, Source[]>;
  settings: Settings;
  selected: RelationRef | null;
  overlay: Overlay;
  /** The tab the Queries dialog showed last, where its sidebar button opens it. Kept in memory only. */
  queriesTab: QueriesTab;
  screen: Screen;
  /** Filters the connection tree in place. Not saved, and kept across a trip to settings. */
  treeQuery: string;
  toast: { id: number; text: string } | null;
  cursor: { line: number; col: number };
};

const DEFAULT_LAYOUT: Layout = {
  sidebar: true,
  results: true,
  rightPanel: null,
  resultsHeight: 320,
  sidebarWidth: 280,
  sidebarSplit: 50,
  rightPanelWidth: 341,
  assistantChat: null,
};
export const DEFAULT_SETTINGS: Settings = {
  assistantAccess: "SchemaOnly",
  maxRows: 200_000,
  maxResultBytes: 512 * 1024 * 1024,
  tablePageSize: 500,
  stopOnError: true,
  colorScheme: "System",
  lightTheme: "l1-light",
  darkTheme: "l1-dark",
  customThemes: [],
  interfaceFont: { family: "Inter", size: 13 },
  codeFont: { family: "JetBrains Mono", size: 13 },
  density: "Standard",
  panelMotionMs: 150,
  reduceMotion: "System",
  tintConnections: false,
  providers: [{ provider: "Claude", enabled: true, displayName: "Claude", binaryPath: "", configDir: "", launchArgs: "" }],
  providerHealthCheckSecs: 300,
};

/**
 * A tab saved before tabs carried a database (`database: null`) resolves to the connection's
 * configured database, the only one it could have used. A tab whose connection is gone keeps its
 * text and becomes a plain query tab with no target.
 */
export function tabFromSnapshot(t: TabSnapshot, configs: Record<ConnectionId, ConnectionConfig>): Tab {
  const config = t.console.connectionId ? configs[t.console.connectionId] : undefined;
  const db = config ? (t.console.database == null ? homeDb(config) : { connectionId: config.id, database: t.console.database }) : null;
  const table = db && t.table && { schema: t.table.schema, table: t.table.table, page: t.table.page, query: t.table.query };
  return {
    id: t.id,
    title: t.title,
    console: { db, mode: t.console.mode, ...(t.console.schema && { schema: t.console.schema }) },
    table,
  };
}

/**
 * A notebook on a connection that is gone has no connection, as its cells do. A file notebook saved clean takes its
 * cells' saved text as what the file holds; boot's read of the file confirms it.
 */
function notebookFromSnapshot(
  n: NotebookSnapshot,
  configs: Record<ConnectionId, ConnectionConfig>,
  tabs: Record<TabId, Tab>,
  texts: Record<TabId, string>,
): Notebook {
  const saved = n.cells.flatMap((id) => {
    const tab = tabs[id];
    return tab ? [{ text: texts[id] ?? "", meta: metaKey(cellMeta(tab)) }] : [];
  });
  const file: NotebookFile | null = n.file && {
    binding: n.file.binding,
    health: "ok",
    plain: n.file.plain,
    unknown: n.file.unknown,
    saved: n.file.dirty ? null : saved,
  };
  return { id: n.id, title: n.title, db: n.db && configs[n.db.connectionId] ? n.db : null, cells: n.cells, active: n.active, shownAtMs: n.shownAtMs, file };
}

/**
 * A workspace saved while a file belonged to one tab: each such cell becomes a notebook of its own on its database, bound
 * to the file and titled with its name. The cell takes a new cell's title, so saving a plain file keeps it plain.
 */
function fileTabNotebooks(w: Workspace, restored: Record<TabId, Tab>): { tabs: Record<TabId, Tab>; notebooks: Notebook[] } {
  let tabs = restored;
  const notebooks: Notebook[] = [];
  for (const t of w.tabs) {
    const tab = tabs[t.id];
    if (!t.file || !tab) continue;
    const cell = { ...tab, title: nextUntitled(Object.values(tabs)) };
    tabs = { ...tabs, [t.id]: cell };
    const saved = t.file.dirty ? null : [{ text: t.text ?? "", meta: metaKey(cellMeta(cell)) }];
    notebooks.push({
      id: crypto.randomUUID(),
      title: fileStem(fileName(t.file.binding.path)),
      db: cell.console.db,
      cells: [t.id],
      active: t.id,
      shownAtMs: 0,
      file: { binding: t.file.binding, health: "ok", plain: true, unknown: { header: [], cells: {} }, saved },
    });
  }
  return { tabs, notebooks };
}

type Shows = Pick<WorkspaceData, "notebooks" | "shown">;

export function shownNotebook(s: Shows): Notebook | undefined {
  return s.shown.kind === "notebook" ? s.notebooks[s.shown.id] : undefined;
}

export function allCells(s: Pick<WorkspaceData, "notebooks">): TabId[] {
  return Object.values(s.notebooks).flatMap((n) => n.cells);
}

export function activeId(s: Shows): TabId | null {
  return shownNotebook(s)?.active ?? null;
}

export function activeTab(s: Shows & Pick<WorkspaceData, "tabs">): Tab | undefined {
  const id = activeId(s);
  return id ? s.tabs[id] : undefined;
}

/** The database the workspace is on: the shown notebook's, or the one the start commands offer a notebook on. */
export function shownDb(s: Shows): DatabaseRef | null {
  return s.shown.kind === "start" ? s.shown.db : (shownNotebook(s)?.db ?? null);
}

/**
 * Cells saved before notebooks, or claimed by no notebook this build could read, join the notebook for their database in
 * their saved order. A cell that moved into a file notebook of its own shows it when it was active; otherwise the saved
 * `shown` wins, then the notebook holding the saved active cell. A workspace from before notebooks shows its first
 * notebook when it saved no active cell.
 */
function restoreNotebooks(w: Workspace, tabs: Record<TabId, Tab>, configs: Record<ConnectionId, ConnectionConfig>, fromFiles: Notebook[]): Shows {
  const texts = Object.fromEntries(w.tabs.map((t) => [t.id, t.text ?? ""]));
  const moved = new Set(fromFiles.flatMap((n) => n.cells));
  const notebooks = settle(
    w.notebooks.map((n) => notebookFromSnapshot(n, configs, tabs, texts)),
    (id) => (moved.has(id) ? undefined : tabs[id]?.console.db),
    w.tabs.map((t) => t.id),
  );
  for (const n of fromFiles) notebooks[n.id] = n;
  const holding = w.active ? notebookOf(notebooks, w.active) : undefined;
  const fromFile = w.active && moved.has(w.active) ? holding : undefined;
  const shown = fromFile ?? (w.shown ? notebooks[w.shown] : undefined) ?? holding ?? (w.notebooks.length === 0 ? Object.values(notebooks)[0] : undefined);
  if (!shown) return { notebooks, shown: NO_DATABASE };
  if (shown === holding && w.active) notebooks[shown.id] = { ...shown, active: w.active };
  return { notebooks, shown: { kind: "notebook", id: shown.id } };
}

export type Buffers = { textOf(id: TabId): string; isNotebookDirty(id: NotebookId): boolean };

/** A file notebook's `dirty` is read from the same buffers as its cells' `text`, so the two always describe one state. */
export function workspaceSnapshot(s: Pick<WorkspaceData, "tabs" | "notebooks" | "shown" | "layout">, buffers: Buffers): Workspace {
  const tabs = allCells(s).flatMap((id): TabSnapshot[] => {
    const t = s.tabs[id];
    if (!t) return [];
    const { db, mode } = t.console;
    return [
      {
        id: t.id,
        title: t.title,
        text: buffers.textOf(t.id),
        console: { connectionId: db?.connectionId ?? null, database: db?.database ?? null, schema: t.console.schema ?? null, mode },
        table: t.table && { schema: t.table.schema, table: t.table.table, page: t.table.page, query: t.table.query },
        file: null,
      },
    ];
  });
  const fileSnapshot = (n: Notebook, file: NotebookFile) => {
    const cells = Object.fromEntries(n.cells.flatMap((id) => (file.unknown.cells[id] ? [[id, file.unknown.cells[id]]] : [])));
    return { binding: file.binding, dirty: buffers.isNotebookDirty(n.id), plain: file.plain, unknown: { header: file.unknown.header, cells } };
  };
  return {
    tabs,
    active: activeId(s),
    layout: s.layout,
    notebooks: Object.values(s.notebooks).map((n) => ({
      id: n.id,
      title: n.title,
      db: n.db,
      cells: n.cells,
      active: n.active,
      shownAtMs: n.shownAtMs,
      file: n.file && fileSnapshot(n, n.file),
    })),
    shown: s.shown.kind === "notebook" ? s.shown.id : null,
  };
}

export function nextUntitled(tabs: Iterable<Tab>): string {
  let max = 0;
  for (const t of tabs) {
    const n = /^Untitled-(\d+)\.sql$/.exec(t.title)?.[1];
    if (n) max = Math.max(max, Number(n));
  }
  return `Untitled-${max + 1}.sql`;
}

export function notebookWithFile(notebooks: Notebooks, path: string): Notebook | undefined {
  return Object.values(notebooks).find((n) => n.file?.binding.path === path);
}

export function tabTarget(tab: Tab | undefined): DatabaseRef | null {
  return tab?.console.db ?? null;
}

type WorkspaceActions = {
  restore(w: Workspace, connections: ConnectionConfig[], settings: Settings): void;
  resolveDatabases(connections: Record<ConnectionId, ConnectionState>): void;
  /** Adds `tab` to the notebook for its database, at the end or right after `after`, and shows it active. */
  addTab(tab: Tab, after?: TabId): void;
  /** Starts a new notebook on the tab's database with `tab` as its first cell, and shows it. */
  addNotebook(tab: Tab): void;
  /** Adds a whole notebook whose `cells` are `tabs`, and shows it. */
  openNotebook(notebook: Notebook, tabs: Tab[]): void;
  /** Makes `tabs` the notebook's cells, in order. A cell left out stays in `tabs` until `closeTab` drops it. */
  replaceCells(id: NotebookId, tabs: Tab[]): void;
  /** Binds the notebook to a file, or replaces what it knows of the file. A `title` renames it in the same step. */
  setNotebookFile(id: NotebookId, file: NotebookFile, title?: string): void;
  patchNotebookFile(id: NotebookId, patch: Partial<Pick<NotebookFile, "health" | "saved">>): void;
  /**
   * Closing a notebook's last cell drops the notebook. When it was shown, the most recent other notebook on its database
   * shows, else the start commands on it.
   */
  closeTab(id: TabId): void;
  setActive(id: TabId): void;
  /** Shows the most recent notebook on `db`, or the start commands on it when it has none. Never touches a cell. */
  showDatabase(db: DatabaseRef | null): void;
  showNotebook(id: NotebookId): void;
  /** A blank title keeps the old one. */
  renameNotebook(id: NotebookId, title: string): void;
  renameTab(id: TabId, title: string): void;
  /** A cell's database is its notebook's, so a patch never moves it. */
  patchConsole(id: TabId, patch: Partial<Pick<ConsoleTarget, "mode" | "schema">>): void;
  setTablePage(id: TabId, page: number, exactRowCount?: number): void;
  setTableQuery(id: TabId, query: TableQuery): void;
  setLayout(patch: Partial<Layout>): void;
  setActiveResult(id: TabId, pick: ResultPick): void;
  setResultDisplay(id: TabId, pref: ResultDisplayPref): void;
  setRunSources(id: TabId, sources: Source[]): void;
  upsertConfig(c: ConnectionConfig): void;
  removeConfig(id: ConnectionId): void;
  setSettings(s: Settings): void;
  select(r: RelationRef | null): void;
  open(o: Overlay): void;
  /** Opens the Queries dialog on `tab`, or on the tab it showed last. */
  openQueries(tab?: QueriesTab): void;
  close(): void;
  answer(choice: string): void;
  openSettings(section?: SettingsSectionId): void;
  closeSettings(): void;
  setTreeQuery(query: string): void;
  setSettingsQuery(query: string): void;
  notify(text: string): void;
  setCursor(cursor: { line: number; col: number }): void;
};

let toastSeq = 0;

/**
 * The notebook a new cell on `db` joins: the one holding `after`, else the shown one, when either is on `db`; else the
 * notebook for `db`, or a new one.
 */
function homeFor(s: Shows, db: DatabaseRef | null, after: TabId | undefined): Notebook {
  const candidates = [after ? notebookOf(s.notebooks, after) : undefined, shownNotebook(s)];
  return candidates.find((n) => n && sameDb(n.db, db)) ?? notebookFor(s.notebooks, db) ?? newNotebook(Object.values(s.notebooks), db);
}

/** Moves each cell whose database changed into the notebook for its new database, and keeps the active cell in view. */
function settled(s: Shows, notebooks: Notebook[], tabs: Record<TabId, Tab>): Shows {
  return followActive(
    s.shown,
    s.notebooks,
    settle(notebooks, (id) => tabs[id]?.console.db),
  );
}

export const useWorkspace = create<WorkspaceData & WorkspaceActions>()((set, get) => ({
  tabs: {},
  notebooks: {},
  shown: NO_DATABASE,
  layout: DEFAULT_LAYOUT,
  configs: {},
  activeResult: {},
  resultDisplay: {},
  runSources: {},
  settings: DEFAULT_SETTINGS,
  selected: null,
  overlay: { kind: "none" },
  queriesTab: "history",
  screen: { kind: "workspace" },
  treeQuery: "",
  toast: null,
  cursor: { line: 1, col: 1 },

  restore: (w, connections, settings) => {
    const configs = Object.fromEntries(connections.map((c) => [c.id, c]));
    const restored = Object.fromEntries(w.tabs.map((t) => [t.id, tabFromSnapshot(t, configs)]));
    const { tabs, notebooks } = fileTabNotebooks(w, restored);
    set({ tabs, ...restoreNotebooks(w, tabs, configs, notebooks), layout: w.layout, configs, settings });
  },
  resolveDatabases: (connections) =>
    set((s) => {
      const resolve = (db: DatabaseRef | null) => db && resolveDb(db, connections[db.connectionId]);
      const tabs = Object.fromEntries(
        Object.entries(s.tabs).map(([id, t]) => {
          const db = resolve(t.console.db);
          return [id, db === t.console.db ? t : { ...t, console: { ...t.console, db } }];
        }),
      );
      const notebooks: Notebook[] = [];
      for (const n of Object.values(s.notebooks)) {
        const db = resolve(n.db);
        const next = db === n.db ? n : { ...n, db };
        // Two spellings of one database resolve to the same one, so their cells belong in one notebook. Notebooks that
        // already shared a database stay apart.
        // A file notebook is its file, so it never merges.
        const into = notebooks.findIndex((m) => sameDb(m.db, next.db) && (next !== n || m !== s.notebooks[m.id]) && !m.file && !next.file);
        const target = notebooks[into];
        if (target) notebooks[into] = { ...target, cells: [...target.cells, ...next.cells], active: target.active ?? next.active };
        else notebooks.push(next);
      }
      const startDb = s.shown.kind === "start" ? resolve(s.shown.db) : null;
      const shown: Shown = s.shown.kind === "start" && startDb !== s.shown.db ? { kind: "start", db: startDb } : s.shown;
      const changed = Object.entries(tabs).some(([id, t]) => t !== s.tabs[id]) || notebooks.some((n) => n !== s.notebooks[n.id]);
      const selectedDb = s.selected && resolveDb(s.selected.db, connections[s.selected.db.connectionId]);
      const selected = s.selected && selectedDb && selectedDb !== s.selected.db ? { ...s.selected, db: selectedDb } : s.selected;
      return {
        ...(changed ? { tabs, ...settled({ notebooks: s.notebooks, shown }, notebooks, tabs) } : shown !== s.shown && { shown }),
        ...(selected !== s.selected && { selected }),
      };
    }),
  addTab: (tab, after) =>
    set((s) => {
      const home = homeFor(s, tab.console.db, after);
      const at = after === undefined ? -1 : home.cells.indexOf(after);
      const cells = at < 0 ? [...home.cells, tab.id] : [...home.cells.slice(0, at + 1), tab.id, ...home.cells.slice(at + 1)];
      return { tabs: { ...s.tabs, [tab.id]: tab }, ...showNotebook({ ...s.notebooks, [home.id]: { ...home, cells, active: tab.id } }, home.id) };
    }),
  addNotebook: (tab) =>
    set((s) => {
      const notebook = newNotebook(Object.values(s.notebooks), tab.console.db);
      return {
        tabs: { ...s.tabs, [tab.id]: tab },
        ...showNotebook({ ...s.notebooks, [notebook.id]: { ...notebook, cells: [tab.id], active: tab.id } }, notebook.id),
      };
    }),
  openNotebook: (notebook, tabs) =>
    set((s) => ({
      tabs: { ...s.tabs, ...Object.fromEntries(tabs.map((t) => [t.id, t])) },
      ...showNotebook({ ...s.notebooks, [notebook.id]: notebook }, notebook.id),
    })),
  replaceCells: (id, tabs) =>
    set((s) => {
      const notebook = s.notebooks[id];
      if (!notebook) return {};
      const cells = tabs.map((t) => t.id);
      const active = notebook.active && cells.includes(notebook.active) ? notebook.active : (cells[0] ?? null);
      return { tabs: { ...s.tabs, ...Object.fromEntries(tabs.map((t) => [t.id, t])) }, notebooks: { ...s.notebooks, [id]: { ...notebook, cells, active } } };
    }),
  setNotebookFile: (id, file, title) =>
    set((s) => {
      const notebook = s.notebooks[id];
      return notebook ? { notebooks: { ...s.notebooks, [id]: { ...notebook, file, ...(title && { title }) } } } : {};
    }),
  patchNotebookFile: (id, patch) =>
    set((s) => {
      const notebook = s.notebooks[id];
      return notebook?.file ? { notebooks: { ...s.notebooks, [id]: { ...notebook, file: { ...notebook.file, ...patch } } } } : {};
    }),
  closeTab: (id) =>
    set((s) => {
      const { [id]: _, ...tabs } = s.tabs;
      const { [id]: __, ...activeResult } = s.activeResult;
      const { [id]: ___, ...resultDisplay } = s.resultDisplay;
      const { [id]: _____, ...runSources } = s.runSources;
      const notebook = notebookOf(s.notebooks, id);
      if (!notebook) return { tabs, activeResult, resultDisplay, runSources };
      const index = notebook.cells.indexOf(id);
      const cells = notebook.cells.filter((c) => c !== id);
      if (cells.length === 0) {
        const { [notebook.id]: ____, ...notebooks } = s.notebooks;
        const wasShown = s.shown.kind === "notebook" && s.shown.id === notebook.id;
        return { tabs, ...(wasShown ? showDatabase(notebooks, notebook.db) : { notebooks }), activeResult, resultDisplay, runSources };
      }
      const active = notebook.active === id ? (cells[Math.min(index, cells.length - 1)] ?? null) : notebook.active;
      return { tabs, notebooks: { ...s.notebooks, [notebook.id]: { ...notebook, cells, active } }, activeResult, resultDisplay, runSources };
    }),
  setActive: (id) =>
    set((s) => {
      const notebook = notebookOf(s.notebooks, id);
      if (!notebook || (notebook.active === id && shownNotebook(s) === notebook)) return {};
      return showNotebook({ ...s.notebooks, [notebook.id]: { ...notebook, active: id } }, notebook.id);
    }),
  showDatabase: (db) =>
    set((s) => {
      const notebook = notebookFor(s.notebooks, db);
      if (notebook) return shownNotebook(s) === notebook ? {} : showNotebook(s.notebooks, notebook.id);
      return s.shown.kind === "start" && sameDb(s.shown.db, db) ? {} : showDatabase(s.notebooks, db);
    }),
  showNotebook: (id) => set((s) => (s.notebooks[id] && shownNotebook(s)?.id !== id ? showNotebook(s.notebooks, id) : {})),
  renameNotebook: (id, title) =>
    set((s) => {
      const notebook = s.notebooks[id];
      const trimmed = title.trim();
      return notebook && trimmed && trimmed !== notebook.title ? { notebooks: { ...s.notebooks, [id]: { ...notebook, title: trimmed } } } : {};
    }),
  renameTab: (id, title) =>
    set((s) => {
      const t = s.tabs[id];
      return t ? { tabs: { ...s.tabs, [id]: { ...t, title } } } : {};
    }),
  patchConsole: (id, patch) =>
    set((s) => {
      const t = s.tabs[id];
      return t ? { tabs: { ...s.tabs, [id]: { ...t, console: { ...t.console, ...patch } } } } : {};
    }),
  setTablePage: (id, page, exactRowCount) =>
    set((s) => {
      const t = s.tabs[id];
      return t?.table ? { tabs: { ...s.tabs, [id]: { ...t, table: { ...t.table, page, ...(exactRowCount !== undefined && { exactRowCount }) } } } } : {};
    }),
  setTableQuery: (id, query) =>
    set((s) => {
      const t = s.tabs[id];
      return t?.table ? { tabs: { ...s.tabs, [id]: { ...t, table: { ...t.table, query, page: 0, exactRowCount: undefined } } } } : {};
    }),
  setLayout: (patch) => set((s) => ({ layout: { ...s.layout, ...patch } })),
  setActiveResult: (id, pick) => set((s) => ({ activeResult: { ...s.activeResult, [id]: pick } })),
  setResultDisplay: (id, pref) =>
    set((s) => {
      const run = pref.key.slice(0, pref.key.lastIndexOf(":"));
      const sameRun = Object.entries(s.resultDisplay[id] ?? {}).filter(([key]) => key.slice(0, key.lastIndexOf(":")) === run);
      return { resultDisplay: { ...s.resultDisplay, [id]: { ...Object.fromEntries(sameRun), [pref.key]: pref } } };
    }),
  setRunSources: (id, sources) => set((s) => (s.tabs[id] ? { runSources: { ...s.runSources, [id]: sources } } : {})),
  upsertConfig: (c) =>
    set((s) => {
      const previous = s.configs[c.id];
      const connection = useLive.getState().connections[c.id];
      // A tab on the connection's configured database follows an edit of that database. A tab bound
      // to a table stays on the database the table lives in.
      const follows = (db: DatabaseRef | null) =>
        db === null || (previous !== undefined && previous.database !== c.database && sameDb(db, homeDb(previous, connection)));
      const tabs = Object.fromEntries(
        Object.entries(s.tabs).map(([id, t]) =>
          t.table === null && follows(t.console.db) ? [id, { ...t, console: { ...t.console, db: homeDb(c, connection) } }] : [id, t],
        ),
      );
      return { configs: { ...s.configs, [c.id]: c }, tabs, ...settled(s, Object.values(s.notebooks), tabs) };
    }),
  removeConfig: (id) =>
    set((s) => {
      const { [id]: _, ...configs } = s.configs;
      return shownDb(s)?.connectionId === id ? { configs, shown: NO_DATABASE } : { configs };
    }),
  setSettings: (settings) => set({ settings }),
  select: (selected) => set({ selected }),
  open: (overlay) => set(overlay.kind === "queries" ? { overlay, queriesTab: overlay.tab } : { overlay }),
  openQueries: (tab = get().queriesTab) => get().open(tab === "history" ? { kind: "queries", tab } : { kind: "queries", tab, entry: { kind: "browse" } }),
  close: () => set({ overlay: { kind: "none" } }),
  answer: (choice) => {
    const overlay = get().overlay;
    if (overlay.kind !== "ask") return;
    overlay.settle(choice);
    set({ overlay: { kind: "none" } });
  },
  openSettings: (section = "general") => set({ screen: { kind: "settings", section, query: "" }, overlay: { kind: "none" } }),
  closeSettings: () => set({ screen: { kind: "workspace" }, overlay: { kind: "none" } }),
  setTreeQuery: (treeQuery) => set({ treeQuery }),
  setSettingsQuery: (query) => set((s) => (s.screen.kind === "settings" ? { screen: { ...s.screen, query } } : {})),
  notify: (text) => set({ toast: { id: ++toastSeq, text } }),
  setCursor: (cursor) => set({ cursor }),
}));

// Whatever replaces or clears a question answers it `null`, so no caller waits forever. A promise keeps only its first
// answer, so the `null` that follows `answer` is ignored.
useWorkspace.subscribe((s, prev) => {
  if (prev.overlay !== s.overlay && prev.overlay.kind === "ask") prev.overlay.settle(null);
});

let askSeq = 0;

/**
 * Shows a question and resolves with the chosen id, or `null`. The caller's continuation runs after the click that
 * answered has closed the dialog, so it can ask the next question straight away.
 */
export function ask<C extends string>(q: Question<C>): Promise<C | null> {
  return new Promise((resolve) => {
    const settle = (choice: string | null) => resolve(choice as C | null);
    useWorkspace.getState().open({
      kind: "ask",
      seq: ++askSeq,
      testId: q.testId ?? "ask",
      title: q.title,
      body: q.body,
      statements: q.statements ?? [],
      rows: q.rows ?? [],
      note: q.note ?? "",
      choices: q.choices,
      settle,
    });
  });
}

/** A question with one danger button, `confirm-btn`, in a `confirm-dialog`. Resolves `true` when it is pressed. */
export async function confirm(q: Pick<Question<string>, "title" | "body" | "statements" | "rows" | "note"> & { confirmLabel: string }): Promise<boolean> {
  return (
    (await ask({
      testId: "confirm",
      title: q.title,
      body: q.body,
      statements: q.statements,
      rows: q.rows,
      note: q.note,
      choices: [{ id: "btn", label: q.confirmLabel, variant: "danger" }],
    })) === "btn"
  );
}
