import { create } from "zustand";
import type {
  ConnectionConfig,
  ConnectionId,
  ConnectionState,
  DatabaseRef,
  FileBinding,
  FileSaved,
  Layout,
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
  order: TabId[];
  active: TabId | null;
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

export type Buffers = { textOf(id: TabId): string; isDirty(id: TabId): boolean };

/** A file tab's `dirty` is read from the same buffer as its `text`, so the two always describe one state. */
export function workspaceSnapshot(s: Pick<WorkspaceData, "tabs" | "order" | "active" | "layout">, buffers: Buffers): Workspace {
  const tabs = s.order.flatMap((id): TabSnapshot[] => {
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
  return { tabs, active: s.active, layout: s.layout };
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
  addTab(tab: Tab): void;
  closeTab(id: TabId): void;
  setActive(id: TabId): void;
  renameTab(id: TabId, title: string): void;
  bindFile(id: TabId, file: FileSaved): void;
  setFileHealth(id: TabId, health: FileHealth): void;
  patchConsole(id: TabId, patch: Partial<ConsoleTarget>): void;
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

export const useWorkspace = create<WorkspaceData & WorkspaceActions>()((set, get) => ({
  tabs: {},
  order: [],
  active: null,
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
    const tabs = w.tabs.map((t) => tabFromSnapshot(t, configs));
    const order = tabs.map((t) => t.id);
    set({
      tabs: Object.fromEntries(tabs.map((t) => [t.id, t])),
      order,
      active: w.active && order.includes(w.active) ? w.active : (order[0] ?? null),
      layout: w.layout,
      configs,
      settings,
    });
  },
  resolveDatabases: (connections) =>
    set((s) => {
      const tabs = { ...s.tabs };
      let changed = false;
      for (const [id, tab] of Object.entries(tabs)) {
        const previous = tab.console.db;
        if (!previous) continue;
        const db = resolveDb(previous, connections[previous.connectionId]);
        if (db !== previous) {
          tabs[id] = { ...tab, console: { ...tab.console, db } };
          changed = true;
        }
      }
      const selectedDb = s.selected && resolveDb(s.selected.db, connections[s.selected.db.connectionId]);
      const selected = s.selected && selectedDb && selectedDb !== s.selected.db ? { ...s.selected, db: selectedDb } : s.selected;
      return { ...(changed && { tabs }), ...(selected !== s.selected && { selected }) };
    }),
  addTab: (tab) => set((s) => ({ tabs: { ...s.tabs, [tab.id]: tab }, order: [...s.order, tab.id], active: tab.id })),
  closeTab: (id) =>
    set((s) => {
      const index = s.order.indexOf(id);
      const order = s.order.filter((t) => t !== id);
      const { [id]: _, ...tabs } = s.tabs;
      const { [id]: __, ...activeResult } = s.activeResult;
      const { [id]: ___, ...resultDisplay } = s.resultDisplay;
      const active = s.active === id ? (order[Math.min(index, order.length - 1)] ?? null) : s.active;
      return { tabs, order, active, activeResult, resultDisplay };
    }),
  setActive: (id) => set({ active: id }),
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
      return {
        configs: { ...s.configs, [c.id]: c },
        tabs: Object.fromEntries(
          Object.entries(s.tabs).map(([id, t]) =>
            t.table === null && follows(t.console.db) ? [id, { ...t, console: { ...t.console, db: homeDb(c, connection) } }] : [id, t],
          ),
        ),
      };
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
