import { create } from "zustand";
import type {
  ConnectionConfig,
  ConnectionId,
  ConnectionState,
  DatabaseRef,
  FileBinding,
  FileSaved,
  Layout,
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
import { followActive, NO_DATABASE, newNotebook, notebookFor, notebookOf, settle, type Notebook, type Notebooks, type Shown } from "./notebooks";
import type { ChartSpec } from "../features/results/chart";
import type { ResultDisplay, ResultPick } from "./status";

/** A tab's console target. A tab never names a connection without a database. */
export type ResultDisplayPref = { key: string; display: ResultDisplay; chart: ChartSpec | null };

export type ConsoleTarget = { db: DatabaseRef | null; mode: TxnMode; schema?: string | null };

/** The table a tab opened from the sidebar browses, in the tab's console database. */
export type TableBrowse = { schema: string; table: string; page: number; query: TableQuery; exactRowCount?: number };
/**
 * What the last read or write found about a tab's file. Not saved: boot reads every file again.
 * `changed`: the file changed on disk while the tab held edits, so the next save asks before overwriting.
 * `unreadable`: the file is there, but reading it failed.
 */
export type FileHealth = "ok" | "missing" | "changed" | "unreadable";
export type TabFile = { binding: FileBinding; health: FileHealth };

/** A tab with a `file` is titled with the file's name: `bindFile` sets both, and `renameTab` refuses a file tab. */
export type Tab = { id: TabId; title: string; console: ConsoleTarget; table: TableBrowse | null; file: TabFile | null };
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

export type Overlay =
  | { kind: "none" }
  | { kind: "history" }
  | { kind: "navigation" }
  | { kind: "commands" }
  | { kind: "usage" }
  | { kind: "saved"; entry: SavedEntry }
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
  choices: readonly Choice<C>[];
};

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
  settings: Settings;
  selected: RelationRef | null;
  overlay: Overlay;
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
    file: t.file && { binding: t.file.binding, health: "ok" },
  };
}

/** A notebook on a connection that is gone has no connection, as its cells do. */
function notebookFromSnapshot(n: NotebookSnapshot, configs: Record<ConnectionId, ConnectionConfig>): Notebook {
  return { id: n.id, title: n.title, db: n.db && configs[n.db.connectionId] ? n.db : null, cells: n.cells, active: n.active };
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
 * their saved order. The saved `shown` wins, then the notebook holding the saved active cell. A workspace from before
 * notebooks shows its first notebook when it saved no active cell.
 */
function restoreNotebooks(w: Workspace, tabs: Record<TabId, Tab>, configs: Record<ConnectionId, ConnectionConfig>): Shows {
  const notebooks = settle(
    w.notebooks.map((n) => notebookFromSnapshot(n, configs)),
    (id) => tabs[id]?.console.db,
    w.tabs.map((t) => t.id),
  );
  const holding = w.active ? notebookOf(notebooks, w.active) : undefined;
  const shown = (w.shown ? notebooks[w.shown] : undefined) ?? holding ?? (w.notebooks.length === 0 ? Object.values(notebooks)[0] : undefined);
  if (!shown) return { notebooks, shown: NO_DATABASE };
  if (shown === holding && w.active) notebooks[shown.id] = { ...shown, active: w.active };
  return { notebooks, shown: { kind: "notebook", id: shown.id } };
}

export type Buffers = { textOf(id: TabId): string; isDirty(id: TabId): boolean };

/** A file tab's `dirty` is read from the same buffer as its `text`, so the two always describe one state. */
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
        file: t.file && { binding: t.file.binding, dirty: buffers.isDirty(t.id) },
      },
    ];
  });
  return {
    tabs,
    active: activeId(s),
    layout: s.layout,
    notebooks: Object.values(s.notebooks).map((n) => ({ id: n.id, title: n.title, db: n.db, cells: n.cells, active: n.active })),
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

export function tabWithFile(tabs: Record<TabId, Tab>, path: string): Tab | undefined {
  return Object.values(tabs).find((t) => t.file?.binding.path === path);
}

export function tabTarget(tab: Tab | undefined): DatabaseRef | null {
  return tab?.console.db ?? null;
}

type WorkspaceActions = {
  restore(w: Workspace, connections: ConnectionConfig[], settings: Settings): void;
  resolveDatabases(connections: Record<ConnectionId, ConnectionState>): void;
  /** Adds `tab` to the notebook for its database, at the end or right after `after`, and shows it active. */
  addTab(tab: Tab, after?: TabId): void;
  /** Closing a notebook's last cell drops the notebook and shows the start commands on its database. */
  closeTab(id: TabId): void;
  setActive(id: TabId): void;
  /** Shows the notebook for `db`, or the start commands on it when it has none. Never touches a cell. */
  showDatabase(db: DatabaseRef | null): void;
  renameTab(id: TabId, title: string): void;
  bindFile(id: TabId, file: FileSaved): void;
  setFileHealth(id: TabId, health: FileHealth): void;
  /** A cell's database is its notebook's, so a patch never moves it. */
  patchConsole(id: TabId, patch: Partial<Pick<ConsoleTarget, "mode" | "schema">>): void;
  setTablePage(id: TabId, page: number, exactRowCount?: number): void;
  setTableQuery(id: TabId, query: TableQuery): void;
  setLayout(patch: Partial<Layout>): void;
  setActiveResult(id: TabId, pick: ResultPick): void;
  setResultDisplay(id: TabId, pref: ResultDisplayPref): void;
  upsertConfig(c: ConnectionConfig): void;
  removeConfig(id: ConnectionId): void;
  setSettings(s: Settings): void;
  select(r: RelationRef | null): void;
  open(o: Overlay): void;
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
  return candidates.find((n) => n && sameDb(n.db, db)) ?? notebookFor(s.notebooks, db) ?? newNotebook(db);
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
  settings: DEFAULT_SETTINGS,
  selected: null,
  overlay: { kind: "none" },
  screen: { kind: "workspace" },
  treeQuery: "",
  toast: null,
  cursor: { line: 1, col: 1 },

  restore: (w, connections, settings) => {
    const configs = Object.fromEntries(connections.map((c) => [c.id, c]));
    const tabs = Object.fromEntries(w.tabs.map((t) => [t.id, tabFromSnapshot(t, configs)]));
    set({ tabs, ...restoreNotebooks(w, tabs, configs), layout: w.layout, configs, settings });
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
        const into = notebooks.findIndex((m) => sameDb(m.db, next.db) && (next !== n || m !== s.notebooks[m.id]));
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
      return {
        tabs: { ...s.tabs, [tab.id]: tab },
        notebooks: { ...s.notebooks, [home.id]: { ...home, cells, active: tab.id } },
        shown: { kind: "notebook", id: home.id },
      };
    }),
  closeTab: (id) =>
    set((s) => {
      const { [id]: _, ...tabs } = s.tabs;
      const { [id]: __, ...activeResult } = s.activeResult;
      const { [id]: ___, ...resultDisplay } = s.resultDisplay;
      const notebook = notebookOf(s.notebooks, id);
      if (!notebook) return { tabs, activeResult, resultDisplay };
      const index = notebook.cells.indexOf(id);
      const cells = notebook.cells.filter((c) => c !== id);
      if (cells.length === 0) {
        const { [notebook.id]: ____, ...notebooks } = s.notebooks;
        const shown = s.shown.kind === "notebook" && s.shown.id === notebook.id ? { kind: "start" as const, db: notebook.db } : s.shown;
        return { tabs, notebooks, shown, activeResult, resultDisplay };
      }
      const active = notebook.active === id ? (cells[Math.min(index, cells.length - 1)] ?? null) : notebook.active;
      return { tabs, notebooks: { ...s.notebooks, [notebook.id]: { ...notebook, cells, active } }, activeResult, resultDisplay };
    }),
  setActive: (id) =>
    set((s) => {
      const notebook = notebookOf(s.notebooks, id);
      if (!notebook || (notebook.active === id && shownNotebook(s) === notebook)) return {};
      return { notebooks: { ...s.notebooks, [notebook.id]: { ...notebook, active: id } }, shown: { kind: "notebook", id: notebook.id } };
    }),
  showDatabase: (db) =>
    set((s) => {
      const notebook = notebookFor(s.notebooks, db);
      if (notebook) return shownNotebook(s) === notebook ? {} : { shown: { kind: "notebook", id: notebook.id } };
      return s.shown.kind === "start" && sameDb(s.shown.db, db) ? {} : { shown: { kind: "start", db } };
    }),
  renameTab: (id, title) =>
    set((s) => {
      const t = s.tabs[id];
      return t && !t.file ? { tabs: { ...s.tabs, [id]: { ...t, title } } } : {};
    }),
  bindFile: (id, file) =>
    set((s) => {
      const t = s.tabs[id];
      return t ? { tabs: { ...s.tabs, [id]: { ...t, title: file.name, file: { binding: file.binding, health: "ok" } } } } : {};
    }),
  setFileHealth: (id, health) =>
    set((s) => {
      const t = s.tabs[id];
      return t?.file && t.file.health !== health ? { tabs: { ...s.tabs, [id]: { ...t, file: { ...t.file, health } } } } : {};
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
      return { configs };
    }),
  setSettings: (settings) => set({ settings }),
  select: (selected) => set({ selected }),
  open: (overlay) => set({ overlay }),
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
      choices: q.choices,
      settle,
    });
  });
}

/** A question with one danger button, `confirm-btn`, in a `confirm-dialog`. Resolves `true` when it is pressed. */
export async function confirm(q: { title: string; body: string; statements?: readonly string[]; confirmLabel: string }): Promise<boolean> {
  return (
    (await ask({
      testId: "confirm",
      title: q.title,
      body: q.body,
      statements: q.statements,
      choices: [{ id: "btn", label: q.confirmLabel, variant: "danger" }],
    })) === "btn"
  );
}
