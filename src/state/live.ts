import { create } from "zustand";
import type {
  AppEvent,
  BootSnapshot,
  ChatId,
  ChatState,
  ChatView,
  ConnectionId,
  ConnectionState,
  ConsoleState,
  DatabaseRef,
  DatabaseState,
  EntrySnapshot,
  HistoryEntry,
  InstanceState,
  OutputEntry,
  ProviderKind,
  ProviderState,
  RunState,
  SavedQueries,
  TabId,
  UpdateState,
} from "../ipc/bindings";
import { dbKey, type DbKey } from "./dbKey";

export const OUTPUT_LIMIT = 5_000;
export type OutputLog = { entries: readonly OutputEntry[]; dropped: number };
const HISTORY_TAIL_LIMIT = 200;

export type LiveData = {
  connections: Record<ConnectionId, ConnectionState>;
  consoles: Record<TabId, ConsoleState>;
  output: Record<TabId, OutputLog>;
  /** Closed tabs, so a late console event cannot bring their state back. */
  forgotten: ReadonlySet<TabId>;
  historyTail: HistoryEntry[];
  providers: Partial<Record<ProviderKind, ProviderState>>;
  chats: Record<ChatId, ChatState>;
  transcripts: Record<ChatId, Record<number, EntrySnapshot>>;
  updates: UpdateState | null;
  savedQueries: SavedQueries;
  /** Shown as `~` at the start of a file's path. */
  homeDir: string | null;
};

type LiveState = LiveData & {
  hydrate(b: BootSnapshot): void;
  hydrateChat(view: ChatView): void;
  hydrateSaved(queries: SavedQueries): void;
  seedTranscript(id: ChatId): void;
  forgetTab(id: TabId): void;
  apply(e: AppEvent): void;
};

export const emptyLive: LiveData = {
  connections: {},
  consoles: {},
  output: {},
  forgotten: new Set(),
  historyTail: [],
  providers: {},
  chats: {},
  transcripts: {},
  updates: null,
  savedQueries: { rev: 0, items: [] },
  homeDir: null,
};

function newer<V extends { rev: number }>(prev: V | null | undefined, next: V): V {
  return prev && prev.rev >= next.rev ? prev : next;
}

function keepHigher<K extends PropertyKey, V extends { rev: number }, M extends Partial<Record<K, V>>>(map: M, key: K, next: V): M {
  const prev = map[key];
  return newer(prev, next) === prev ? map : { ...map, [key]: next };
}

function withChat(s: LiveData, chat: ChatState): LiveData {
  const chats = keepHigher(s.chats, chat.id, chat);
  if (chats[chat.id]?.status.kind !== "Deleted") return { ...s, chats };
  const { [chat.id]: _, ...transcripts } = s.transcripts;
  return { ...s, chats, transcripts };
}

function withEntries(s: LiveData, chatId: ChatId, held: Record<number, EntrySnapshot>, entries: EntrySnapshot[]): LiveData {
  const transcript = entries.reduce((t, entry) => keepHigher(t, entry.seq, entry), held);
  return { ...s, transcripts: { ...s.transcripts, [chatId]: transcript } };
}

export function instanceOf(state: ConnectionState | undefined): InstanceState | null {
  return state?.status.kind === "Connected" ? state.status.instance : null;
}

/** The version `catalog_get` serves for a database: the Ready one, or the last good one behind a failed reload. */
function servedVersion(catalog: DatabaseState["catalog"]): number | null {
  switch (catalog.kind) {
    case "Ready":
      return catalog.version;
    case "Failed":
      return catalog.version;
    case "Idle":
    case "Loading":
      return null;
    default: {
      const unreachable: never = catalog;
      return unreachable;
    }
  }
}

/** Every served catalog across the connected instances: what the catalog store must hold, and at which version. */
export function wantedCatalogs(connections: Record<ConnectionId, ConnectionState>): Map<DbKey, { ref: DatabaseRef; version: number }> {
  const wanted = new Map<DbKey, { ref: DatabaseRef; version: number }>();
  for (const conn of Object.values(connections)) {
    for (const db of instanceOf(conn)?.databases ?? []) {
      const version = servedVersion(db.catalog);
      if (version === null) continue;
      const ref = { connectionId: conn.id, database: db.name };
      wanted.set(dbKey(ref), { ref, version });
    }
  }
  return wanted;
}

export function servedCatalogVersion(connections: Record<ConnectionId, ConnectionState>, db: DatabaseRef): number | undefined {
  return wantedCatalogs(connections).get(dbKey(db))?.version;
}

export function applyEvent(s: LiveData, e: AppEvent): LiveData {
  switch (e.kind) {
    case "ConnectionChanged": {
      const { kind: _, ...state } = e;
      return { ...s, connections: keepHigher(s.connections, state.id, state) };
    }
    case "ConsoleChanged": {
      const { kind: _, ...state } = e;
      if (s.forgotten.has(state.tabId)) return s;
      return { ...s, consoles: keepHigher(s.consoles, state.tabId, state) };
    }
    case "ConsoleOutput": {
      if (s.forgotten.has(e.tabId)) return s;
      const prev = s.output[e.tabId] ?? { entries: [], dropped: 0 };
      const all = prev.entries.concat(e.entries);
      const cut = Math.max(0, all.length - OUTPUT_LIMIT);
      return { ...s, output: { ...s.output, [e.tabId]: { entries: all.slice(cut), dropped: prev.dropped + cut } } };
    }
    case "HistoryAppended": {
      const { kind: _, ...entry } = e;
      return { ...s, historyTail: [entry, ...s.historyTail].slice(0, HISTORY_TAIL_LIMIT) };
    }
    case "ProviderChanged": {
      const { kind: _, ...state } = e;
      return { ...s, providers: keepHigher(s.providers, state.provider, state) };
    }
    case "ChatChanged": {
      const { kind: _, ...state } = e;
      return withChat(s, state);
    }
    case "ChatEntryChanged": {
      const { kind: _, ...entry } = e;
      const held = s.transcripts[entry.chatId];
      return held ? withEntries(s, entry.chatId, held, [entry]) : s;
    }
    case "UpdateChanged": {
      const { kind: _, ...state } = e;
      return { ...s, updates: newer(s.updates, state) };
    }
    case "SavedQueriesChanged": {
      const { kind: _, ...queries } = e;
      return hydrateSavedLive(s, queries);
    }
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function hydrateLive(s: LiveData, b: BootSnapshot): LiveData {
  let next = s;
  for (const c of b.live) next = { ...next, connections: keepHigher(next.connections, c.id, c) };
  for (const c of b.consoles) if (!next.forgotten.has(c.tabId)) next = { ...next, consoles: keepHigher(next.consoles, c.tabId, c) };
  for (const p of b.providers) next = { ...next, providers: keepHigher(next.providers, p.provider, p) };
  for (const c of b.chats) next = withChat(next, c);
  return { ...next, updates: newer(next.updates, b.updates), savedQueries: newer(next.savedQueries, b.savedQueries), homeDir: b.homeDir };
}

export function hydrateSavedLive(s: LiveData, queries: SavedQueries): LiveData {
  const savedQueries = newer(s.savedQueries, queries);
  return savedQueries === s.savedQueries ? s : { ...s, savedQueries };
}

export function hydrateChatLive(s: LiveData, view: ChatView): LiveData {
  const next = withChat(s, view.state);
  if (next.chats[view.state.id]?.status.kind === "Deleted") return next;
  return withEntries(next, view.state.id, next.transcripts[view.state.id] ?? {}, view.entries);
}

export function forgetTabLive(s: LiveData, id: TabId): LiveData {
  const { [id]: _console, ...consoles } = s.consoles;
  const { [id]: _output, ...output } = s.output;
  return { ...s, consoles, output, forgotten: new Set(s.forgotten).add(id) };
}

export function seedTranscriptLive(s: LiveData, id: ChatId): LiveData {
  if (s.transcripts[id] || s.chats[id]?.status.kind === "Deleted") return s;
  return { ...s, transcripts: { ...s.transcripts, [id]: {} } };
}

export const useLive = create<LiveState>()((set) => ({
  ...emptyLive,
  hydrate: (b) => set((s) => hydrateLive(s, b)),
  hydrateChat: (view) => set((s) => hydrateChatLive(s, view)),
  hydrateSaved: (queries) => set((s) => hydrateSavedLive(s, queries)),
  seedTranscript: (id) => set((s) => seedTranscriptLive(s, id)),
  forgetTab: (id) => set((s) => forgetTabLive(s, id)),
  apply: (e) => set((s) => applyEvent(s, e)),
}));

export type Ending = { kind: "finished"; run: Extract<RunState, { kind: "Finished" }> } | { kind: "gone" };

/**
 * Resolves when the tab's console reports run `runId` Finished, or `gone` once the tab is closed or a newer run replaced
 * it. It reads the store first, because Finished can land before `consoleRun` returns.
 */
export function whenFinished(tabId: TabId, runId: number): Promise<Ending> {
  const ending = (s: LiveData): Ending | null => {
    if (s.forgotten.has(tabId)) return { kind: "gone" };
    const run = s.consoles[tabId]?.run;
    if (run?.kind === "Finished" && run.runId === runId) return { kind: "finished", run };
    return run && run.kind !== "Idle" && run.runId > runId ? { kind: "gone" } : null;
  };
  return new Promise((resolve) => {
    const now = ending(useLive.getState());
    if (now) return resolve(now);
    const stop = useLive.subscribe((s) => {
      const later = ending(s);
      if (!later) return;
      stop();
      resolve(later);
    });
  });
}
