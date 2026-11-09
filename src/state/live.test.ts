import { describe, expect, it } from "vitest";
import type {
  AppEvent,
  BootSnapshot,
  ChatState,
  ChatStatus,
  ConnectionState,
  ConsoleState,
  DatabaseState,
  EntrySnapshot,
  OutputEntry,
  ProviderState,
  SavedQueries,
  UpdateState,
  UpdateStatus,
} from "../ipc/bindings";
import { applyEvent, emptyLive, hydrateChatLive, hydrateLive, hydrateSavedLive, OUTPUT_LIMIT, seedTranscriptLive, wantedCatalogs, type LiveData } from "./live";

const CONN = "c0000000-0000-4000-8000-000000000001";
const TAB = "t0000000-0000-4000-8000-000000000001";

function consoleAt(rev: number, txn: ConsoleState["txn"] = "Idle"): ConsoleState {
  return {
    tabId: TAB,
    rev,
    config: { connectionId: CONN, database: "shop", schema: "public", mode: "Manual" },
    session: { kind: "None" },
    txn,
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
}

const consoleChanged = (c: ConsoleState): AppEvent => ({ kind: "ConsoleChanged", ...c });

const CHAT = "a0000000-0000-4000-8000-000000000001";
const OTHER_CHAT = "a0000000-0000-4000-8000-000000000002";
const RUNNING: ChatStatus = { kind: "Running", turn: 0, startedAtMs: 5, phase: "Working" };

function chatAt(rev: number, status: ChatStatus = { kind: "Idle" }, id = CHAT): ChatState {
  return {
    id,
    rev,
    title: "Top customers",
    updatedAtMs: 1000,
    options: { provider: "Claude", model: "opus", effort: "high", permission: "AskFirst" },
    status,
    entryCount: 2,
  };
}

function entryAt(seq: number, rev: number, text: string, chatId = CHAT): EntrySnapshot {
  return { chatId, seq, rev, turn: 0, entry: { kind: "Notice", text } };
}

function providerAt(rev: number, checking: boolean): ProviderState {
  const info = {
    defaultName: "Claude",
    defaultBinary: "claude",
    configEnv: "CLAUDE_CONFIG_DIR",
    configDirDefault: "~/.claude",
    signInCommand: "claude auth login",
    ownedFlags: [],
  };
  return { provider: "Claude", rev, info, ignoredFlags: [], checking, checkedAtMs: null, outcome: null };
}

const applyAll = (s: LiveData, events: AppEvent[]) => events.reduce(applyEvent, s);

function savedAt(rev: number, names: string[]): SavedQueries {
  return {
    rev,
    items: names.map((name, i) => ({ id: i + 1, name, sql: `select ${i}`, databaseKind: "Postgres", excerpt: `select ${i}`, createdAtMs: 1, usedAtMs: 1 })),
  };
}

function bootWith(savedQueries: SavedQueries): BootSnapshot {
  return {
    connections: [],
    unreadableConnections: 0,
    homeDir: null,
    live: [],
    consoles: [],
    providers: [],
    chats: [],
    workspace: {
      tabs: [],
      active: null,
      layout: {
        sidebar: true,
        results: true,
        rightPanel: null,
        resultsHeight: 320,
        sidebarWidth: 256,
        sidebarSplit: 50,
        rightPanelWidth: 341,
        assistantChat: null,
      },
      notebooks: [],
      shown: null,
    },
    settings: {
      maxRows: 1,
      maxResultBytes: 1,
      tablePageSize: 1,
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
      providers: [],
      providerHealthCheckSecs: 300,
      assistantAccess: "SchemaOnly",
    },
    updates: updateAt(0, { kind: "Idle" }),
    savedQueries,
  };
}

function updateAt(rev: number, status: UpdateStatus): UpdateState {
  return { rev, track: "Stable", currentVersion: "0.1.0", status, checkedAtMs: null };
}

describe("live.apply", () => {
  it("keeps the higher rev regardless of arrival order", () => {
    const inOrder = [consoleAt(1), consoleAt(2, "Active")].reduce((s, c) => applyEvent(s, consoleChanged(c)), emptyLive);
    const reversed = [consoleAt(2, "Active"), consoleAt(1)].reduce((s, c) => applyEvent(s, consoleChanged(c)), emptyLive);
    expect(inOrder.consoles[TAB]?.txn).toBe("Active");
    expect(reversed.consoles[TAB]?.txn).toBe("Active");
    expect(reversed.consoles[TAB]?.rev).toBe(2);
  });

  it("is idempotent for a replayed snapshot", () => {
    const once = applyEvent(emptyLive, consoleChanged(consoleAt(3, "Active")));
    expect(applyEvent(once, consoleChanged(consoleAt(3, "Active")))).toEqual(once);
  });

  it("strips the event tag so the stored value is the contract's ConsoleState", () => {
    const s = applyEvent(emptyLive, consoleChanged(consoleAt(1)));
    expect(s.consoles[TAB]).toEqual(consoleAt(1));
  });

  it("an event that raced the boot snapshot survives hydrate when it is newer", () => {
    const early = applyEvent(emptyLive, {
      kind: "ConnectionChanged",
      id: CONN,
      rev: 9,
      status: {
        kind: "Connected",
        server: { version: "16.4", versionNum: 160004, encoding: "UTF8" },
        instance: { configuredDatabase: "shop", home: "shop", showSystemDatabases: false, databases: [] },
      },
    });
    const boot: BootSnapshot = {
      connections: [],
      unreadableConnections: 0,
      homeDir: null,
      live: [{ id: CONN, rev: 4, status: { kind: "Connecting" } }],
      consoles: [consoleAt(1)],
      workspace: {
        tabs: [],
        active: null,
        layout: {
          sidebar: true,
          results: true,
          rightPanel: null,
          resultsHeight: 320,
          sidebarWidth: 256,
          sidebarSplit: 50,
          rightPanelWidth: 341,
          assistantChat: null,
        },
        notebooks: [],
        shown: null,
      },
      settings: {
        maxRows: 1,
        maxResultBytes: 1,
        tablePageSize: 1,
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
        providers: [],
        providerHealthCheckSecs: 300,
        assistantAccess: "SchemaOnly",
      },
      providers: [],
      chats: [],
      updates: updateAt(0, { kind: "Idle" }),
      savedQueries: savedAt(1, []),
    };
    const s = hydrateLive(early, boot);
    expect(s.connections[CONN]?.status.kind).toBe("Connected");
    expect(s.consoles[TAB]?.rev).toBe(1);
    expect(s.updates).toEqual(updateAt(0, { kind: "Idle" }));
  });

  it("hydrate takes providers, chats, and updates from boot, and keeps newer ones that raced it", () => {
    const checking = updateAt(4, { kind: "Checking" });
    const early = applyAll(emptyLive, [
      { kind: "ChatChanged", ...chatAt(5, RUNNING) },
      { kind: "UpdateChanged", ...checking },
    ]);
    const boot = hydrateLive(early, {
      connections: [],
      unreadableConnections: 0,
      homeDir: null,
      live: [],
      consoles: [],
      workspace: {
        tabs: [],
        active: null,
        layout: {
          sidebar: true,
          results: true,
          rightPanel: null,
          resultsHeight: 320,
          sidebarWidth: 256,
          sidebarSplit: 50,
          rightPanelWidth: 341,
          assistantChat: CHAT,
        },
        notebooks: [],
        shown: null,
      },
      settings: {
        maxRows: 1,
        maxResultBytes: 1,
        tablePageSize: 1,
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
        providers: [],
        providerHealthCheckSecs: 300,
        assistantAccess: "SchemaOnly",
      },
      providers: [providerAt(1, true)],
      chats: [chatAt(0), chatAt(0, { kind: "Idle" }, OTHER_CHAT)],
      updates: updateAt(0, { kind: "Idle" }),
      savedQueries: savedAt(1, []),
    });
    expect(boot.providers).toEqual({ Claude: providerAt(1, true) });
    expect(boot.chats).toEqual({ [CHAT]: chatAt(5, RUNNING), [OTHER_CHAT]: chatAt(0, { kind: "Idle" }, OTHER_CHAT) });
    expect(boot.updates).toEqual(checking);
  });

  it("keeps the higher provider snapshot and strips the event tag", () => {
    const s = applyAll(emptyLive, [
      { kind: "ProviderChanged", ...providerAt(2, false) },
      { kind: "ProviderChanged", ...providerAt(1, true) },
    ]);
    expect(s.providers).toEqual({ Claude: providerAt(2, false) });
  });

  it("keeps the saved queries with the higher rev across boot, events, and replies in any order", () => {
    const one = savedAt(2, ["Active sessions"]);
    const two = savedAt(3, ["Active sessions", "Locks"]);
    const raced = applyEvent(emptyLive, { kind: "SavedQueriesChanged", ...two });
    expect(hydrateLive(raced, bootWith(savedAt(1, []))).savedQueries).toEqual(two);
    expect(hydrateLive(emptyLive, bootWith(savedAt(1, []))).savedQueries).toEqual(savedAt(1, []));
    const replyFirst = applyEvent(hydrateSavedLive(emptyLive, two), { kind: "SavedQueriesChanged", ...one });
    const eventFirst = hydrateSavedLive(applyEvent(emptyLive, { kind: "SavedQueriesChanged", ...one }), two);
    expect([replyFirst.savedQueries, eventFirst.savedQueries]).toEqual([two, two]);
    expect(hydrateSavedLive(replyFirst, two)).toBe(replyFirst);
  });

  it("keeps the update snapshot with the higher rev", () => {
    const found = updateAt(3, { kind: "Available", version: "0.2.0", downgrade: false, changes: [] });
    const s = applyAll(emptyLive, [
      { kind: "UpdateChanged", ...found },
      { kind: "UpdateChanged", ...updateAt(2, { kind: "Checking" }) },
    ]);
    expect(s.updates).toEqual(found);
  });

  it("keeps the higher chat snapshot regardless of arrival order", () => {
    const s = applyAll(emptyLive, [
      { kind: "ChatChanged", ...chatAt(3, RUNNING) },
      { kind: "ChatChanged", ...chatAt(2) },
    ]);
    expect(s.chats).toEqual({ [CHAT]: chatAt(3, RUNNING) });
  });

  it("applies entry events only to a transcript it holds, keeping the higher rev per seq", () => {
    const held = hydrateChatLive(emptyLive, { state: chatAt(1), entries: [entryAt(0, 1, "first")] });
    const s = applyAll(held, [
      { kind: "ChatEntryChanged", ...entryAt(1, 4, "Hel") },
      { kind: "ChatEntryChanged", ...entryAt(1, 3, "H") },
      { kind: "ChatEntryChanged", ...entryAt(0, 6, "not held", OTHER_CHAT) },
    ]);
    expect(s.transcripts).toEqual({ [CHAT]: { 0: entryAt(0, 1, "first"), 1: entryAt(1, 4, "Hel") } });
  });

  it("hydrateChat loads a chat whole and keeps an entry that streamed in newer", () => {
    let s = hydrateChatLive(emptyLive, { state: chatAt(2), entries: [entryAt(0, 1, "first")] });
    s = applyEvent(s, { kind: "ChatEntryChanged", ...entryAt(1, 9, "newer") });
    s = hydrateChatLive(s, { state: chatAt(3), entries: [entryAt(0, 1, "first"), entryAt(1, 8, "older")] });
    expect(s.chats).toEqual({ [CHAT]: chatAt(3) });
    expect(s.transcripts).toEqual({ [CHAT]: { 0: entryAt(0, 1, "first"), 1: entryAt(1, 9, "newer") } });
  });

  it("a seeded transcript keeps an entry event that raced the open, and the older view hydrated on top does not replace it", () => {
    let s = seedTranscriptLive(emptyLive, CHAT);
    s = applyEvent(s, { kind: "ChatEntryChanged", ...entryAt(1, 9, "newer") });
    s = hydrateChatLive(s, { state: chatAt(3), entries: [entryAt(0, 1, "first"), entryAt(1, 8, "older")] });
    expect(s.transcripts).toEqual({ [CHAT]: { 0: entryAt(0, 1, "first"), 1: entryAt(1, 9, "newer") } });
  });

  it("seeding leaves a held transcript as it is and holds nothing for a deleted chat", () => {
    const held = hydrateChatLive(emptyLive, { state: chatAt(1), entries: [entryAt(0, 1, "first")] });
    const deleted = applyEvent(emptyLive, { kind: "ChatChanged", ...chatAt(7, { kind: "Deleted" }) });
    expect([seedTranscriptLive(held, CHAT).transcripts, seedTranscriptLive(deleted, CHAT).transcripts]).toEqual([
      { [CHAT]: { 0: entryAt(0, 1, "first") } },
      {},
    ]);
  });

  it("a deleted chat drops its transcript, and nothing older brings it back", () => {
    const held = hydrateChatLive(emptyLive, { state: chatAt(1), entries: [entryAt(0, 1, "first")] });
    expect(Object.keys(held.transcripts)).toEqual([CHAT]);
    let s = applyEvent(held, { kind: "ChatChanged", ...chatAt(7, { kind: "Deleted" }) });
    s = applyAll(s, [
      { kind: "ChatChanged", ...chatAt(6) },
      { kind: "ChatEntryChanged", ...entryAt(1, 5, "late") },
    ]);
    s = hydrateChatLive(s, { state: chatAt(6), entries: [entryAt(0, 1, "first")] });
    expect(s.chats).toEqual({ [CHAT]: chatAt(7, { kind: "Deleted" }) });
    expect(s.transcripts).toEqual({});
  });

  it("wants the Ready catalogs of connected instances and the last good one behind a failed reload", () => {
    const db = (name: string, catalog: DatabaseState["catalog"]): DatabaseState => ({
      name,
      system: false,
      access: "Ok",
      shown: name === "shop",
      showSystemSchemas: false,
      shownSchemas: [],
      schemaCount: { kind: "Unknown" },
      catalog,
    });
    const server = { version: "16.4", versionNum: 160004, encoding: "UTF8" };
    const connections: Record<string, ConnectionState> = {
      [CONN]: {
        id: CONN,
        rev: 3,
        status: {
          kind: "Connected",
          server,
          instance: {
            configuredDatabase: "shop",
            home: "shop",
            showSystemDatabases: false,
            databases: [
              db("postgres", { kind: "Idle" }),
              db("reporting", { kind: "Ready", version: 7 }),
              db("shop", { kind: "Loading" }),
              db("stale", { kind: "Failed", message: "statement timeout", version: 5 }),
              db("never", { kind: "Failed", message: "permission denied", version: null }),
            ],
          },
        },
      },
      other: {
        id: "other",
        rev: 1,
        status: {
          kind: "Connected",
          server,
          instance: { configuredDatabase: "x", home: "x", showSystemDatabases: false, databases: [db("x", { kind: "Ready", version: 2 })] },
        },
      },
      down: { id: "down", rev: 9, status: { kind: "Lost", reason: "gone" } },
    };
    expect([...wantedCatalogs(connections).entries()]).toEqual([
      [`${CONN}\u0000reporting`, { ref: { connectionId: CONN, database: "reporting" }, version: 7 }],
      [`${CONN}\u0000stale`, { ref: { connectionId: CONN, database: "stale" }, version: 5 }],
      ["other\u0000x", { ref: { connectionId: "other", database: "x" }, version: 2 }],
    ]);
  });

  it("appends output and caps it per tab", () => {
    const entry = (n: number): OutputEntry => ({ atMs: n, runId: 1, body: { kind: "Info", message: `m${n}` } });
    let s = applyEvent(emptyLive, { kind: "ConsoleOutput", tabId: TAB, entries: [entry(0), entry(1)] });
    s = applyEvent(s, { kind: "ConsoleOutput", tabId: TAB, entries: Array.from({ length: OUTPUT_LIMIT }, (_, i) => entry(i + 2)) });
    const out = s.output[TAB]!;
    expect(out.entries).toHaveLength(OUTPUT_LIMIT);
    expect(out.entries[0]?.atMs).toBe(2);
    expect(out.entries.at(-1)?.atMs).toBe(OUTPUT_LIMIT + 1);
    expect(out.dropped).toBe(2);
  });

  it("prepends appended history entries", () => {
    const base = {
      databaseKind: "Postgres" as const,
      atMs: 0,
      connectionId: CONN,
      connectionName: "p",
      database: "shop",
      sql: "select 1",
      elapsedMs: 1,
      outcome: { kind: "Cancelled" as const },
    };
    let s = applyEvent(emptyLive, { kind: "HistoryAppended", id: 1, ...base });
    s = applyEvent(s, { kind: "HistoryAppended", id: 2, ...base });
    expect(s.historyTail.map((h) => h.id)).toEqual([2, 1]);
  });
});
