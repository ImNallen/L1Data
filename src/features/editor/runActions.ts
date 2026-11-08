import { Prec, type EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import {
  commands,
  type ConsoleConfig,
  type DatabaseRef,
  type DatabaseKind,
  type ResultRef,
  type RunMode,
  type RunRequest,
  type RunTarget,
  type Settings,
  type TabId,
  type TableQuery,
  type TableScript,
  type TxnMode,
  type WriteConfirmation,
} from "../../ipc/bindings";
import { applyErrorText, configureErrorText, runErrorText, txnErrorText } from "../../ipc/errors";
import { databaseKindFor } from "../../state/databaseKind";
import { homeDb, resolveDb, sameDb } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { canCancel, isBusy } from "../../state/status";
import { NO_EDITS, pendingCount, toChanges, useTableEdits, type TableEdits } from "../../state/tableEdits";
import {
  EMPTY_QUERY,
  activeId,
  activeTab,
  confirm,
  isTableTab,
  nextUntitled,
  tabTarget,
  useWorkspace,
  type ConsoleTarget,
  type RelationRef,
  type Tab,
  type TableTab,
} from "../../state/workspace";
import {
  applyFormatEdit,
  cursorAnchor,
  cursorOf,
  dropEditor,
  editorStateOf,
  flashRanges,
  installEditorBehavior,
  onBuffersChanged,
  seedEditor,
  showDiagnostics,
  showRunTarget,
  textOf,
} from "./editorStates";
import { closeChooser, openChooser } from "./chooserState";
import { cancelStaleParams, finishParams, forgetParams, promptParams } from "./paramsState";
import { docText } from "./docText";

const ws = () => useWorkspace.getState();
const notify = (text: string) => ws().notify(text);

function consoleConfig({ db, mode, schema }: ConsoleTarget): ConsoleConfig | null {
  return db && { connectionId: db.connectionId, database: db.database, schema: schema ?? null, mode };
}

function sameConfig(a: ConsoleConfig, b: ConsoleConfig): boolean {
  return a.connectionId === b.connectionId && a.database === b.database && a.schema === b.schema && a.mode === b.mode;
}

export async function ensureConsole(tab: Tab): Promise<boolean> {
  const config = consoleConfig(tab.console);
  if (!config) {
    notify("Choose a connection for this tab.");
    return false;
  }
  const live = useLive.getState().consoles[tab.id];
  if (!live || !sameConfig(live.config, config)) {
    const res = await commands.consoleConfigure(tab.id, config);
    if (res.status === "error") {
      notify(configureErrorText(res.error));
      return false;
    }
  }
  return ws().tabs[tab.id]?.console === tab.console;
}

const runIntents = new Map<TabId, symbol>();

const sent = new Map<TabId, { runId: number; text: string }>();

const tableScripts = new Map<string, TableScript>();
const tableScriptKey = (db: DatabaseRef, schema: string, table: string) => JSON.stringify([db.connectionId, databaseKindFor(db, ws().configs), schema, table]);

export async function browseTemplate(db: DatabaseRef, schema: string, table: string): Promise<string | null> {
  const key = tableScriptKey(db, schema, table);
  const cached = tableScripts.get(key);
  if (cached !== undefined) return cached.text;
  const res = await commands.tableScript(db, schema, table);
  if (key !== tableScriptKey(db, schema, table)) return null;
  if (res.status === "error") {
    notify(res.error);
    return null;
  }
  tableScripts.set(key, res.data);
  return res.data.text;
}

export function isBrowsing(tab: Tab): tab is TableTab {
  return (
    isTableTab(tab) &&
    tableScripts.get(tableScriptKey(tab.console.db, tab.table.schema, tab.table.table))?.acceptedTexts.includes(textOf(tab.id).trim()) === true
  );
}

export function textTarget(tab: Tab): { db: DatabaseRef | null; databaseKind: DatabaseKind } {
  const db = tabTarget(tab);
  return { db, databaseKind: databaseKindFor(db, ws().configs) };
}

function sameTextTarget(id: TabId, target: ReturnType<typeof textTarget>): boolean {
  const tab = ws().tabs[id];
  return tab !== undefined && sameDb(tabTarget(tab), target.db) && textTarget(tab).databaseKind === target.databaseKind;
}

export function selectionTarget(state: EditorState): RunTarget {
  const sel = state.selection.main;
  return sel.empty ? { kind: "cursor", offset: sel.head } : { kind: "selection", from: sel.from, to: sel.to };
}

/** Formats the statements the selection touches, or the statement ⌘↵ would run, as one undoable edit. */
export async function formatActive(): Promise<void> {
  const id = activeId(ws());
  const tab = id ? ws().tabs[id] : undefined;
  if (!id || !tab || isBrowsing(tab)) return;
  const state = editorStateOf(id);
  const target = textTarget(tab);
  const res = await commands.editorFormat(state.doc.toString(), selectionTarget(state), target.databaseKind);
  if (activeId(ws()) !== id || !sameTextTarget(id, target)) return;
  if (res.status === "error") {
    if (res.error.kind === "Unsafe") notify("Couldn't format this statement safely.");
    if (res.error.kind === "Unsupported") notify("Formatting is not supported for this SQL construct.");
    return;
  }
  const { from, to, insert } = res.data;
  const stale = editorStateOf(id).doc !== state.doc;
  const unchanged = state.doc.sliceString(from, to) === insert;
  if (stale || unchanged) return;
  applyFormatEdit(id, res.data);
}

type CountContext = { tab: TableTab; text: string; pageSize: number; intent: symbol };
type PageTarget = { page: number; exactRowCount?: number; countContext?: CountContext; intent: symbol };
type PageQueue = { intent: symbol; pendingNativeCount?: symbol; target?: PageTarget };

function isCurrentCount({ tab, text, pageSize, intent }: CountContext): boolean {
  const current = ws().tabs[tab.id];
  return (
    queuedPages.get(tab.id)?.intent === intent &&
    isTableTab(current) &&
    current.console === tab.console &&
    current.table === tab.table &&
    textOf(tab.id) === text &&
    ws().settings.tablePageSize === pageSize
  );
}

const queuedPages = new Map<TabId, PageQueue>();

function nextPageIntent(tabId: TabId): PageQueue {
  const queue: PageQueue = { intent: Symbol(), pendingNativeCount: queuedPages.get(tabId)?.pendingNativeCount };
  queuedPages.set(tabId, queue);
  return queue;
}

function deferPageForPendingWork(tabId: TabId, target: PageTarget | undefined): boolean {
  const queue = queuedPages.get(tabId);
  if (!isBusy(useLive.getState().consoles[tabId]) && !queue?.pendingNativeCount) return false;
  if (target && queue) queue.target = target;
  return true;
}

/** A held write may still be open or may have committed already, and a run may be in either mode, so each sentence must hold for all of them. */
function heldWrites(asked: WriteConfirmation, outcome: (it: string) => string = covered): string {
  const one = asked.holds.count === 1;
  return [
    `${one ? "A write" : `${asked.holds.count} writes`} to ${asked.connection} / ${asked.database} ran on this tab before the connection was marked Production.`,
    `If ${one ? "it is" : "they are"} still in this tab's transaction, ${outcome(one ? "it" : "them")}.`,
  ].join(" ");
}

function covered(it: string): string {
  return `confirming covers ${it}, so a later commit does not ask about ${it} again`;
}

function shownWrites(asked: WriteConfirmation): { statements: string[]; note: string } {
  const statements = [...asked.holds.statements, ...asked.sends.statements];
  const total = asked.holds.count + asked.sends.count;
  return { statements, note: total > statements.length ? `${statements.length} of ${total} are shown.` : "" };
}

/** The answer holds only for the target it named, so a target changed meanwhile drops the run. */
async function writesConfirmed(tab: Tab, asked: WriteConfirmation, mode: RunMode): Promise<boolean> {
  const n = asked.sends.count;
  const h = asked.holds.count;
  const unit = asked.unit === "Batch" ? (n === 1 ? "batch" : "batches") : n === 1 ? "statement" : "statements";
  const { statements, note } = shownWrites(asked);
  const sqlServerAnalyze = mode === "ExplainAnalyze" && databaseKindFor(tabTarget(tab), ws().configs) === "SqlServer";
  const offProduction = sqlServerAnalyze && !asked.onProduction;
  const body = [
    offProduction
      ? `Explain Analyze executes this batch on ${asked.connection} / ${asked.database}. This app does not roll it back.`
      : n > 0 && `This run sends ${n} ${unit} that ${n === 1 ? "writes" : "write"} to ${asked.connection} / ${asked.database}.`,
    h > 0 && heldWrites(asked),
    note,
    mode === "ExplainAnalyze" && !sqlServerAnalyze && "A rollback does not undo sequence values or outside side effects.",
  ];
  const ok = await confirm({
    title: offProduction ? "Run this batch?" : n > 0 ? `Run ${plural(n, "write")} on Production?` : "Run on Production?",
    body: body.filter(Boolean).join(" "),
    statements,
    confirmLabel: offProduction ? "Run" : n > 0 ? "Run writes" : "Run",
  });
  return ok && ws().tabs[tab.id]?.console === tab.console;
}

export function explainSupported(kind: DatabaseKind): boolean {
  switch (kind) {
    case "Postgres":
    case "SqlServer":
      return true;
  }
}

async function runTab(tabId: TabId, target?: PageTarget, script?: RunTarget, mode: RunMode = "Run"): Promise<void> {
  const tab = ws().tabs[tabId];
  if (!tab) return;
  if (target && queuedPages.get(tabId)?.intent !== target.intent) return;
  if (target?.countContext && !isCurrentCount(target.countContext)) return;
  if (target && !isBrowsing(tab)) return;
  if (deferPageForPendingWork(tabId, target)) return;
  const intent = Symbol();
  runIntents.set(tabId, intent);
  finishParams(null);
  const state = editorStateOf(tabId);
  const destination = textTarget(tab);
  const browsing = isBrowsing(tab);
  const valid = () =>
    runIntents.get(tabId) === intent &&
    (browsing || activeId(ws()) === tabId) &&
    ws().tabs[tabId]?.console === tab.console &&
    editorStateOf(tabId).doc === state.doc &&
    (browsing || editorStateOf(tabId).selection.eq(state.selection)) &&
    sameTextTarget(tabId, destination) &&
    (browsing || (ws().overlay.kind === "none" && ws().screen.kind === "workspace"));
  if (!(await ensureConsole(tab)) || (target && !isBrowsing(tab))) return;
  if (target && queuedPages.get(tabId)?.intent !== target.intent) return;
  if (target?.countContext && !isCurrentCount(target.countContext)) return;
  if (deferPageForPendingWork(tabId, target)) return;

  if (!valid()) return;
  let request: RunRequest;
  let text = "";
  if (isBrowsing(tab)) {
    const { schema, table, page, query } = tab.table;
    request = { kind: "tablePage", schema, table, page: target?.page ?? page, query };
  } else {
    text = state.doc.toString();
    const runTarget = script ?? selectionTarget(state);
    const discovered = await commands.scriptParams(text, runTarget, destination.databaseKind);
    if (!valid()) return;
    if (discovered.status === "error") {
      notify(runErrorText(discovered.error));
      return;
    }
    const db = destination.db;
    const targetKey = JSON.stringify([db, destination.databaseKind]);
    const description = db ? `${ws().configs[db.connectionId]?.name ?? db.connectionId} / ${db.database}` : "";
    const params = discovered.data.length ? await promptParams(tabId, targetKey, discovered.data, description, valid) : [];
    if (!params || !valid()) return;
    request = { kind: "script", text, target: runTarget, mode, params };
    showDiagnostics(tabId, []);
  }

  let res = await commands.consoleRun(tabId, request, null);
  while (res.status === "error" && res.error.kind === "ConfirmWrites") {
    if (!(await writesConfirmed(tab, res.error.confirmation, mode)) || !valid()) return;
    res = await commands.consoleRun(tabId, request, res.error.confirmation.digest);
  }
  if (res.status === "error") {
    notify(runErrorText(res.error));
    return;
  }
  if (target?.countContext && !isCurrentCount(target.countContext)) return;
  showResults();
  if (target && request.kind === "tablePage") ws().setTablePage(tabId, target.page, target.exactRowCount);
  if (request.kind === "script") {
    sent.set(tabId, { runId: res.data.runId, text });
    flashRanges(tabId, res.data.statements);
    showRunErrors(useLive.getState());
  }
}

export function explainActive(mode: "Explain" | "ExplainAnalyze"): void {
  const id = activeId(ws());
  const tab = id ? ws().tabs[id] : undefined;
  if (!id || !tab || isBrowsing(tab) || !explainSupported(databaseKindFor(tabTarget(tab), ws().configs))) return;
  void runTab(id, undefined, undefined, mode);
}

/** Runs the selection or the statement at the cursor, or `script` when given. A browsing table tab loads its page. */
export function runActive(script?: RunTarget): void {
  const id = activeId(ws());
  if (id) void runTab(id, undefined, script);
}

/**
 * Runs the selection or the statement at the cursor. With no statement at the cursor (a blank line), or with `ask` and several
 * statements, it opens the chooser instead.
 */
export async function runOrChoose(ask = false): Promise<void> {
  const id = activeId(ws());
  const tab = id ? ws().tabs[id] : undefined;
  if (!id || !tab) return;
  const state = editorStateOf(id);
  const sel = state.selection.main;
  if (isBrowsing(tab) || !sel.empty) return runActive();
  if (isBusy(useLive.getState().consoles[id]) || !(await ensureConsole(tab))) return;
  const text = state.doc.toString();
  const target = textTarget(tab);
  const outline = await commands.scriptOutline(text, sel.head, target.databaseKind);
  const now = editorStateOf(id);
  if (activeId(ws()) !== id || !sameTextTarget(id, target) || now.doc !== state.doc || !now.selection.main.empty || now.selection.main.head !== sel.head)
    return;
  const { statements, atCursor } = outline;
  if (statements.length === 0 || (atCursor !== null && (!ask || statements.length < 2))) return runTab(id, undefined, { kind: "cursor", offset: sel.head });
  const anchor = cursorAnchor();
  if (!anchor) return;
  const above = statements.findLastIndex((r) => r.to <= sel.head);
  openChooser({ tabId: id, text, statements, unitKind: outline.unitKind, active: { kind: "statement", index: atCursor ?? Math.max(above, 0) }, anchor });
}

export function cancelActive(): boolean {
  const id = activeId(ws());
  if (!id || !canCancel(useLive.getState().consoles[id])) return false;
  queuedPages.delete(id);
  if (databaseKindFor(tabTarget(ws().tabs[id]), ws().configs) === "SqlServer" && useLive.getState().consoles[id]?.work !== "Import")
    notify("Cancelling SQL Server resets this session. Earlier commits remain committed; the last transaction outcome may be unknown.");
  void commands.consoleCancel(id);
  return true;
}

async function commitConfirmed(tab: Tab, asked: WriteConfirmation): Promise<boolean> {
  const { statements, note } = shownWrites(asked);
  const ok = await confirm({
    title: "Commit on Production?",
    body: [heldWrites(asked, (it) => `this commit keeps ${it}`), note].filter(Boolean).join(" "),
    statements,
    confirmLabel: "Commit writes",
  });
  return ok && ws().tabs[tab.id]?.console === tab.console;
}

export async function endTransaction(tabId: TabId, verb: "commit" | "rollback"): Promise<void> {
  if (verb === "rollback") {
    const res = await commands.consoleRollback(tabId);
    if (res.status === "error") notify(txnErrorText(res.error));
    return;
  }
  const tab = ws().tabs[tabId];
  let res = await commands.consoleCommit(tabId, null);
  while (tab && res.status === "error" && res.error.kind === "ConfirmWrites") {
    if (!(await commitConfirmed(tab, res.error.confirmation))) return;
    res = await commands.consoleCommit(tabId, res.error.confirmation.digest);
  }
  if (res.status === "error") notify(txnErrorText(res.error));
}

async function reconfigure(tabId: TabId, patch: Partial<Pick<ConsoleTarget, "mode" | "schema">>) {
  const tab = ws().tabs[tabId];
  if (!tab) return;
  const target = { ...tab.console, ...patch };
  if (target.mode === "Manual" && databaseKindFor(target.db, ws().configs) === "SqlServer") {
    notify("SQL Server Manual mode is unavailable. Choose Auto-commit and use native transaction statements.");
    return;
  }
  const config = consoleConfig(target);
  if (config && useLive.getState().consoles[tabId]) {
    const res = await commands.consoleConfigure(tabId, config);
    if (res.status === "error") {
      notify(configureErrorText(res.error));
      return;
    }
  }
  ws().patchConsole(tabId, patch);
}

export const setTabSchema = (tabId: TabId, schema: string | null) => reconfigure(tabId, { schema });

export const setTabMode = (tabId: TabId, mode: TxnMode) => reconfigure(tabId, { mode });

function defaultTarget(): DatabaseRef | null {
  const s = ws();
  if (s.shown.kind === "notebook") return s.notebooks[s.shown.id]?.db ?? null;
  if (s.shown.db) return s.shown.db;
  const live = useLive.getState().connections;
  const configs = Object.values(s.configs);
  const config = configs.find((c) => live[c.id]?.status.kind === "Connected") ?? configs[0];
  return config ? homeDb(config, live[config.id]) : null;
}

/** Opens a SQL cell in the notebook for its database, at the end or right after `after`, and shows that notebook. */
/** `newNotebook` starts a notebook with the cell even when its database has one. */
export function openSqlTab(
  opts: { db?: DatabaseRef | null; schema?: string | null; text?: string; title?: string; after?: TabId; newNotebook?: boolean } = {},
): TabId {
  const id = crypto.randomUUID();
  const inheritedSchema = activeTab(ws())?.console.schema;
  const schema = opts.schema !== undefined ? opts.schema : opts.db === undefined ? inheritedSchema : undefined;
  const target = opts.db === undefined ? defaultTarget() : opts.db;
  const db = target && resolveDb(target, useLive.getState().connections[target.connectionId]);
  seedEditor(id, opts.text ?? "");
  const tab: Tab = {
    id,
    title: opts.title ?? nextUntitled(Object.values(ws().tabs)),
    console: { db, mode: "AutoCommit", ...(schema && { schema }) },
    table: null,
    file: null,
  };
  if (opts.newNotebook) ws().addNotebook(tab);
  else ws().addTab(tab, opts.after);
  return id;
}

/** Opens the rows behind one row of a query result in a new cell after the result's own, and runs it. */
export async function openDrill(r: ResultRef, row: number): Promise<void> {
  const tab = ws().tabs[r.tabId];
  if (!tab) return;
  const res = await commands.resultDrill(r, { kind: "Row", row, title: tab.title });
  if (res.status === "error") {
    notify(res.error.kind === "Unsupported" ? res.error.reason : "The result is gone. Run the query again.");
    return;
  }
  openSqlTab({ db: res.data.db, schema: res.data.schema, text: res.data.sql, after: tab.id });
  runActive({ kind: "all" });
}

function showResults() {
  if (!ws().layout.results) ws().setLayout({ results: true });
}

export async function openTableTab(ref: RelationRef, query?: TableQuery): Promise<TabId | null> {
  const reusable = (t: TableTab) => query === undefined || (pendingCount(useTableEdits.getState().byTab[t.id] ?? NO_EDITS) === 0 && isBrowsing(t));
  const findOpen = () =>
    Object.values(ws().tabs).find(
      (t): t is TableTab => isTableTab(t) && sameDb(t.console.db, ref.db) && t.table.schema === ref.schema && t.table.table === ref.relation && reusable(t),
    );
  const show = (tab: TableTab) => {
    ws().setActive(tab.id);
    if (query) updateTableQuery(tab.id, () => query);
    return tab.id;
  };
  const existing = findOpen();
  ws().select(ref);
  showResults();
  if (existing) return show(existing);
  const template = await browseTemplate(ref.db, ref.schema, ref.relation);
  if (template === null) return null;
  const opened = findOpen();
  if (opened) return show(opened);
  const id = crypto.randomUUID();
  seedEditor(id, template);
  ws().addTab({
    id,
    title: ref.relation,
    console: { db: ref.db, mode: "AutoCommit" },
    table: { schema: ref.schema, table: ref.relation, page: 0, query: query ?? EMPTY_QUERY },
    file: null,
  });
  return id;
}

const loadedTables = new Set<TabId>();

// Loads only as a page, so a restored tab whose saved text is no longer the browse query never runs that text unasked.
export async function loadTableOnce(tabId: TabId): Promise<void> {
  const tab = ws().tabs[tabId];
  if (loadedTables.has(tabId) || !isTableTab(tab)) return;
  const key = tableScriptKey(tab.console.db, tab.table.schema, tab.table.table);
  const template = await browseTemplate(tab.console.db, tab.table.schema, tab.table.table);
  const current = ws().tabs[tabId];
  if (
    template === null ||
    loadedTables.has(tabId) ||
    !isTableTab(current) ||
    !sameDb(current.console.db, tab.console.db) ||
    key !== tableScriptKey(current.console.db, current.table.schema, current.table.table) ||
    current.table !== tab.table
  )
    return;
  loadedTables.add(tabId);
  if (useLive.getState().consoles[tabId]?.run.kind !== "Finished") setTablePage(tabId, tab.table.page);
}

// The store updates before the IPC so a second change made meanwhile builds on this one.
// Writes run in order, including every access change that must revoke an older turn.
let settingsWrites: Promise<void> = Promise.resolve();

export async function flushSettings(): Promise<void> {
  let pending: Promise<void>;
  do {
    pending = settingsWrites;
    await pending;
  } while (pending !== settingsWrites);
}

export async function saveSettings(patch: Partial<Settings>): Promise<void> {
  const prev = ws().settings;
  const next = { ...prev, ...patch };
  ws().setSettings(next);
  const write = settingsWrites.catch(() => {}).then(() => commands.settingsSet(next));
  settingsWrites = write;
  await write;
  if (next.tablePageSize === prev.tablePageSize) return;
  for (const tab of Object.values(ws().tabs)) if (isTableTab(tab)) setTablePage(tab.id, 0);
}

export async function lastTablePage(tabId: TabId): Promise<void> {
  const tab = ws().tabs[tabId];
  if (!isTableTab(tab) || !isBrowsing(tab) || isBusy(useLive.getState().consoles[tabId]) || queuedPages.get(tabId)?.pendingNativeCount) return;
  const { schema, table, query } = tab.table;
  const queue = nextPageIntent(tabId);
  const { intent } = queue;
  const countContext = { tab, text: textOf(tabId), pageSize: ws().settings.tablePageSize, intent };
  if (databaseKindFor(tab.console.db, ws().configs) === "SqlServer") queue.pendingNativeCount = intent;
  try {
    const res = await commands.exactRowCount(tabId, tab.console.db, schema, table, query);
    const current = queuedPages.get(tabId);
    if (current?.pendingNativeCount === intent) delete current.pendingNativeCount;
    if (!isCurrentCount(countContext)) return;
    if (res.status === "error") {
      notify(res.error.kind === "NotConnected" ? "Connect to jump to the last page." : res.error.message);
      return;
    }
    await runTab(tabId, { page: Math.max(0, Math.ceil(res.data / countContext.pageSize) - 1), exactRowCount: res.data, countContext, intent });
  } finally {
    const current = queuedPages.get(tabId);
    if (current?.pendingNativeCount === intent) delete current.pendingNativeCount;
    runQueuedPages(useLive.getState());
  }
}

export function setTablePage(tabId: TabId, page: number, exactRowCount?: number): void {
  const { intent } = nextPageIntent(tabId);
  void runTab(tabId, { page, exactRowCount, intent });
}

export function updateTableQuery(tabId: TabId, update: (q: TableQuery) => TableQuery): void {
  const tab = ws().tabs[tabId];
  if (!isTableTab(tab)) return;
  ws().setTableQuery(tabId, update(tab.table.query));
  setTablePage(tabId, 0);
}

export const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export async function submitEdits(tabId: TabId): Promise<void> {
  const tab = ws().tabs[tabId];
  const edits = useTableEdits.getState().byTab[tabId];
  if (!isTableTab(tab) || !edits || pendingCount(edits) === 0) return;
  const store = useTableEdits.getState();
  if (!store.beginSubmit(tabId)) return;
  try {
    await applyEdits(tab, edits);
  } finally {
    store.endSubmit(tabId);
  }
}

async function editsConfirmed(tab: TableTab, edits: TableEdits, asked: WriteConfirmation): Promise<boolean> {
  const updates = Object.keys(edits.updates).filter((k) => !(k in edits.deletes)).length;
  const parts = [
    edits.inserts.length > 0 && plural(edits.inserts.length, "insert"),
    updates > 0 && plural(updates, "update"),
    Object.keys(edits.deletes).length > 0 && plural(Object.keys(edits.deletes).length, "delete"),
  ].filter((p): p is string => p !== false);
  const what = parts.length > 1 ? `${parts.slice(0, -1).join(", ")} and ${parts.at(-1)}` : parts[0];
  const ok = await confirm({
    title: `Apply ${plural(asked.sends.count, "change")} on Production?`,
    body: [
      `This sends ${what} to ${tab.table.schema}.${tab.table.table} on ${asked.connection} / ${asked.database}.`,
      asked.holds.count > 0 && heldWrites(asked),
    ]
      .filter(Boolean)
      .join(" "),
    statements: asked.holds.statements,
    confirmLabel: "Apply changes",
  });
  const now = ws().tabs[tab.id];
  return ok && now?.console === tab.console && useTableEdits.getState().byTab[tab.id] === edits;
}

export async function revertEdits(tabId: TabId): Promise<void> {
  const edits = useTableEdits.getState().byTab[tabId];
  const n = edits ? pendingCount(edits) : 0;
  if (n === 0) return;
  const ok = await confirm({
    title: `Revert ${plural(n, "change")}?`,
    body: "The staged changes are discarded. Nothing has been sent to the database.",
    confirmLabel: "Revert",
  });
  if (ok && useTableEdits.getState().byTab[tabId] === edits) useTableEdits.getState().clear(tabId);
}

async function applyEdits(tab: TableTab, edits: TableEdits): Promise<void> {
  const tabId = tab.id;
  if (!(await ensureConsole(tab))) return;
  const count = pendingCount(edits);
  const { changes, rowOf } = toChanges(edits, tab.table.schema, tab.table.table);
  let res = await commands.tableApply(tabId, changes, null);
  while (res.status === "error" && res.error.kind === "ConfirmWrites") {
    if (!(await editsConfirmed(tab, edits, res.error.confirmation))) return;
    res = await commands.tableApply(tabId, changes, res.error.confirmation.digest);
  }
  if (res.status === "error") {
    const failedAt = "change" in res.error ? rowOf(res.error.change) : null;
    useTableEdits.setState((s) => ({ byTab: { ...s.byTab, [tabId]: { ...edits, failedAt } } }));
    notify(applyErrorText(res.error));
    return;
  }
  useTableEdits.getState().clear(tabId);
  notify(res.data.committed ? `Saved ${plural(count, "change")}.` : `${plural(count, "change")} wait in the open transaction. Commit to keep them.`);
  setTablePage(tabId, tab.table.page);
}

export function forgetTab(tabId: TabId) {
  runIntents.delete(tabId);
  forgetParams(tabId);
  sent.delete(tabId);
  queuedPages.delete(tabId);
  loadedTables.delete(tabId);
  dropRowCaches(tabId);
  dropEditor(tabId);
  useTableEdits.getState().clear(tabId);
  ws().closeTab(tabId);
}

// The Finished event can arrive before or after consoleRun returns, so both paths call this.
function showRunErrors(s: ReturnType<typeof useLive.getState>) {
  for (const [tabId, req] of sent) {
    const run = s.consoles[tabId]?.run;
    if (run?.kind !== "Finished" || run.runId !== req.runId) continue;
    sent.delete(tabId);
    if (textOf(tabId) !== req.text) continue;
    const failed = run.statements.find((st) => st.status.kind === "Failed");
    if (failed?.status.kind !== "Failed") continue;
    const { position, message } = failed.status.error;
    if (position === null) continue;
    const word = /^[\w."$]+/.exec(req.text.slice(position))?.[0].length ?? 1;
    const from = Math.min(position, req.text.length);
    showDiagnostics(tabId, [{ from, to: Math.min(from + word, req.text.length), severity: "error", message }]);
  }
}

const docListeners = new Set<() => void>();
export function onEditorDocChanged(listener: () => void): () => void {
  docListeners.add(listener);
  return () => docListeners.delete(listener);
}

let markTimer: ReturnType<typeof setTimeout> | undefined;
let marking = false;
let markAgain = false;

function refreshRunMark(): void {
  clearTimeout(markTimer);
  markTimer = setTimeout(() => void markRunTarget(), 30);
}

// One outline request at a time; changes made while it is out run one more pass when it returns.
async function markRunTarget(): Promise<void> {
  if (marking) {
    markAgain = true;
    return;
  }
  marking = true;
  try {
    do {
      markAgain = false;
      await markOnce();
    } while (markAgain);
  } finally {
    marking = false;
  }
}

async function markOnce(): Promise<void> {
  const id = activeId(ws());
  const tab = id ? ws().tabs[id] : undefined;
  if (!id || !tab) return;
  const state = editorStateOf(id);
  const sel = state.selection.main;
  if (isBrowsing(tab) || !sel.empty) return showRunTarget(id, null);
  const target = textTarget(tab);
  const outline = await commands.scriptOutline(docText(state.doc), sel.head, target.databaseKind);
  const now = editorStateOf(id);
  // A stale reply is dropped: the change that made it stale already scheduled a newer pass.
  if (activeId(ws()) !== id || !sameTextTarget(id, target) || now.doc !== state.doc || !now.selection.main.empty || now.selection.main.head !== sel.head)
    return;
  showRunTarget(id, outline.atCursor === null ? null : (outline.statements[outline.atCursor] ?? null));
}

function runQueuedPages(s: ReturnType<typeof useLive.getState>, previous?: ReturnType<typeof useLive.getState>) {
  for (const [tabId, queue] of queuedPages) {
    const console = s.consoles[tabId];
    if (!ws().tabs[tabId] || !console || (previous?.consoles[tabId]?.session.kind === "Open" && console.session.kind !== "Open")) {
      queuedPages.delete(tabId);
      continue;
    }
    if (isBusy(console) || queue.pendingNativeCount || !queue.target) continue;
    const target = queue.target;
    delete queue.target;
    if (console.session.kind !== "Open") continue;
    void runTab(tabId, target);
  }
}

export function installRunBehavior(): void {
  onBuffersChanged(cancelStaleParams);
  useLive.subscribe(showRunErrors);
  useLive.subscribe(runQueuedPages);
  useWorkspace.subscribe((s, prev) => {
    const active = activeId(s);
    const before = activeId(prev);
    if (active !== before && before) {
      const previous = s.tabs[before];
      if (previous && !isBrowsing(previous)) runIntents.delete(before);
    }
    for (const [id, before] of Object.entries(prev.tabs)) {
      const now = s.tabs[id];
      if (!now) runIntents.delete(id);
      if (!now || !sameDb(tabTarget(now), tabTarget(before)) || databaseKindFor(tabTarget(now), s.configs) !== databaseKindFor(tabTarget(before), prev.configs))
        forgetParams(id);
    }
    cancelStaleParams();
    const tab = activeTab(s);
    const was = activeTab(prev);
    if (
      active !== before ||
      !sameDb(tabTarget(tab), tabTarget(was)) ||
      databaseKindFor(tabTarget(tab), s.configs) !== databaseKindFor(tabTarget(was), prev.configs)
    ) {
      closeChooser();
      if (active) showRunTarget(active, null);
      refreshRunMark();
    }
  });
  installEditorBehavior([
    Prec.highest(
      keymap.of([
        { key: "Mod-Enter", run: () => (void runOrChoose(), true) },
        { key: "Mod-Shift-Enter", run: () => (void runOrChoose(true), true) },
        { key: "Mod-Alt-Enter", run: () => (explainActive("Explain"), true) },
        { key: "Mod-Alt-Shift-Enter", run: () => (explainActive("ExplainAnalyze"), true) },
        { key: "Mod-Alt-l", run: () => (void formatActive(), true) },
        { key: "Escape", run: () => cancelActive() },
      ]),
    ),
    EditorView.updateListener.of((u) => {
      if (u.docChanged) for (const l of docListeners) l();
      if (u.docChanged || u.selectionSet) {
        const tab = activeTab(ws());
        if (tab && !isBrowsing(tab)) runIntents.delete(tab.id);
        cancelStaleParams();
        ws().setCursor(cursorOf(u.state));
        refreshRunMark();
      }
    }),
  ]);
}
