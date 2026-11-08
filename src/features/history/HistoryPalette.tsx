import * as Dialog from "@radix-ui/react-dialog";
import { BookmarkPlusIcon, CheckIcon, ClockIcon, CopyIcon, PlayIcon } from "lucide-react";
import { Fragment, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { commands, type HistoryFilter, type HistoryOutcome } from "../../ipc/bindings";
import { isMac } from "../../shell/hostedWindow";
import { sameDb } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { activeTab, confirm, tabTarget, useWorkspace } from "../../state/workspace";
import { day } from "../../ui/day";
import { ACTIVE_OPTION, Button, cx, DialogSurface, Kbd, DatabaseBadge, StatusDot } from "../../ui/primitives";
import { focusEditor } from "../editor/editorStates";
import { SqlText } from "../editor/SqlText";
import { formatElapsed, pluralRows } from "../results/format";
import { saveHistoryQuery } from "../saved/savedActions";
import { insertHistoryQuery, openHistoryQuery } from "./historyActions";
import { historyKey, type HistoryAction } from "./historyList";
import { useHistory } from "./useHistory";

function outcomeText(o: HistoryOutcome): string {
  switch (o.kind) {
    case "Rows":
      return pluralRows(o.rows);
    case "Command":
      return o.tag;
    case "Error":
      return o.message;
    case "Cancelled":
      return "Cancelled";
    default: {
      const unreachable: never = o;
      return unreachable;
    }
  }
}

const OUTCOME_LABEL = { Rows: "Succeeded", Command: "Succeeded", Error: "Failed", Cancelled: "Cancelled" } satisfies Record<HistoryOutcome["kind"], string>;

/** `labelled` names the outcome where the dot is its only sign; beside the outcome's text it is decoration. */
function OutcomeDot({ outcome, labelled }: { outcome: HistoryOutcome; labelled?: boolean }) {
  const label = labelled ? OUTCOME_LABEL[outcome.kind] : undefined;
  if (outcome.kind === "Cancelled")
    return (
      <span
        role={label ? "img" : undefined}
        aria-label={label}
        aria-hidden={label ? undefined : true}
        title={label}
        className="inline-block size-1.5 shrink-0 rounded-full border border-faint"
      />
    );
  return <StatusDot tone={outcome.kind === "Error" ? "error" : "live"} label={label} />;
}

const time = (at: number) => new Date(at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });

async function clearHistory() {
  if (
    !(await confirm({ title: "Clear query history?", body: "This removes every query from history. Saved queries are kept.", confirmLabel: "Clear history" }))
  )
    return;
  try {
    await commands.historyClear();
    useLive.setState({ historyTail: [] });
    useWorkspace.getState().open({ kind: "history" });
  } catch (e) {
    useWorkspace.getState().notify(String(e));
  }
}

function HistoryBrowser({ close, restoreEditorFocus }: { close: () => void; restoreEditorFocus: () => void }) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<HistoryFilter>("All");
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const history = useHistory(query, filter);
  const { list, selected, detail, error } = history;
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const active = useWorkspace(activeTab);
  const activeDb = active ? tabTarget(active) : null;
  const insertEnabled = active !== undefined;
  const mod = isMac(navigator.userAgent) ? "⌘" : "Ctrl+";
  // oxlint-disable-next-line react/purity -- relative times read the clock on each render
  const now = new Date();
  const count = list ? { All: list.counts.all, Failed: list.counts.failed, Writes: list.counts.writes }[filter] : null;

  useEffect(() => {
    listRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  }, [selected?.id]);
  useEffect(() => {
    listRef.current?.scrollTo({ top: 0 });
  }, [query, filter]);
  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect -- copy feedback belongs to the selected entry
    setCopied(false);
    setCopyError(false);
  }, [selected?.id]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);

  async function copy() {
    if (!detail) return;
    try {
      await navigator.clipboard.writeText(detail.sql);
      setCopied(true);
      setCopyError(false);
    } catch {
      setCopyError(true);
    }
  }
  function act(action: HistoryAction) {
    if (action === "next" || action === "previous") {
      if (!list) return;
      const index = list.groups.findIndex((g) => g.id === selected?.id);
      const next = list.groups[Math.max(0, Math.min(list.groups.length - 1, index + (action === "next" ? 1 : -1)))];
      if (next) history.setSelectedId(next.id);
      if (action === "next" && index >= list.groups.length - 2) history.loadOlder();
      return;
    }
    if (!selected || !detail) return;
    if (action === "copy") {
      void copy();
      return;
    }
    if (action === "insert") {
      if (!insertHistoryQuery(detail.sql)) return;
      restoreEditorFocus();
      close();
      return;
    }
    openHistoryQuery(selected, detail.sql, action === "run");
    restoreEditorFocus();
    close();
  }
  function onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    const search = searchRef.current;
    const searchHasSelection = document.activeElement === search && search?.selectionStart !== search?.selectionEnd;
    const action = historyKey(e.nativeEvent, searchHasSelection);
    if (!action) return;
    const target = e.target;
    if (target instanceof HTMLElement) {
      if ((action === "next" || action === "previous") && target.closest('[data-testid="history-sql"]')) return;
      if (action === "open" && target.closest('button:not([role="option"])')) return;
    }
    e.preventDefault();
    e.stopPropagation();
    act(action);
  }

  return (
    <div className="flex h-full min-h-0 flex-col" onKeyDown={onKeyDown}>
      <div className="flex min-h-14 shrink-0 items-center gap-3 border-b border-control px-4">
        <ClockIcon size={18} className="shrink-0 text-muted" aria-hidden />
        <input
          ref={searchRef}
          data-testid="history-input"
          autoFocus
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          role="combobox"
          aria-expanded={true}
          aria-autocomplete="list"
          aria-label="Search query history"
          aria-controls="history-list"
          aria-activedescendant={selected ? `history-group-${selected.id}` : undefined}
          placeholder="Search SQL, tables, errors or connections"
          className="h-12 min-w-0 flex-1 bg-transparent text-ui-14 text-fg outline-none placeholder:text-dim"
        />
        <div role="group" aria-label="Filter history" className="flex shrink-0 gap-1">
          {(["All", "Failed", "Writes"] satisfies HistoryFilter[]).map((value) => (
            <button
              key={value}
              type="button"
              data-testid={`history-filter-${value.toLowerCase()}`}
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
              className={cx(
                "flex h-7 items-center gap-1.5 rounded-md bg-pill px-2 text-ui-12 outline-none focus-visible:ring-1 focus-visible:ring-focus",
                filter === value ? "bg-selected text-fg" : "text-dim hover:bg-hover hover:text-fg",
              )}
            >
              {value}
              <span className="text-dim tabular-nums">
                {list ? { All: list.counts.all, Failed: list.counts.failed, Writes: list.counts.writes }[value].toLocaleString() : "…"}
              </span>
            </button>
          ))}
        </div>
        <Dialog.Close asChild>
          <button
            type="button"
            data-testid="history-close"
            aria-label="Close history"
            className="ml-1 border-l border-control pl-3 text-dim outline-none hover:text-fg focus-visible:ring-1 focus-visible:ring-focus"
          >
            <Kbd>esc</Kbd>
          </button>
        </Dialog.Close>
      </div>
      {error && (
        <div role="alert" className="flex shrink-0 items-center justify-between gap-3 border-b border-control px-4 py-2 text-ui-12 text-danger">
          <span className="truncate">{error}</span>
          <Button data-testid="history-retry" variant="ghost" onClick={history.reload}>
            Try again
          </Button>
        </div>
      )}
      <div className="grid min-h-0 flex-1 grid-cols-[35%_minmax(0,1fr)]">
        <div
          ref={listRef}
          id="history-list"
          role="listbox"
          aria-label="Query history"
          aria-activedescendant={selected ? `history-group-${selected.id}` : undefined}
          aria-busy={!list}
          data-testid="history-list"
          tabIndex={0}
          onScroll={(e) => {
            const el = e.currentTarget;
            if (el.scrollHeight - el.scrollTop - el.clientHeight < 160) history.loadOlder();
          }}
          className="min-w-0 overflow-x-hidden overflow-y-auto border-r border-control p-1.5 outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-focus"
        >
          {!list && (
            <p role="status" className="px-3 py-8 text-center text-ui-13 text-dim">
              {error ? "History unavailable" : "Loading history…"}
            </p>
          )}
          {list?.groups.length === 0 && (
            <p role="status" className="px-3 py-8 text-center text-ui-13 text-dim">
              {query.trim() || filter !== "All" ? "No matching queries" : "No queries yet"}
            </p>
          )}
          {list?.groups.map((group, index) => {
            const label = day(group.latestAtMs, now);
            const previous = list.groups[index - 1];
            return (
              <Fragment key={group.id}>
                {(!previous || day(previous.latestAtMs, now) !== label) && (
                  <div role="presentation" className="px-2.5 pt-3 pb-1.5 text-ui-11.5 text-dim">
                    {label}
                  </div>
                )}
                <button
                  type="button"
                  role="option"
                  aria-selected={selected?.id === group.id}
                  id={`history-group-${group.id}`}
                  data-testid="history-item"
                  data-history-id={group.id}
                  tabIndex={-1}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => history.setSelectedId(group.id)}
                  onDoubleClick={() => {
                    if (detail?.id === group.id) act("open");
                  }}
                  className={cx(
                    "block w-full min-w-0 cursor-default rounded-lg px-2.5 py-2.5 text-left outline-none",
                    selected?.id === group.id ? ACTIVE_OPTION : "hover:bg-hover",
                  )}
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <OutcomeDot outcome={group.representative.outcome} labelled />
                    <span className="min-w-0 flex-1 truncate text-ui-13 font-medium text-fg" title={group.title}>
                      {group.title}
                    </span>
                    {group.runCount > 1 && (
                      <span data-testid="history-repeat" className="shrink-0 rounded bg-pill px-1 text-ui-11.5 text-dim">
                        ×{group.runCount.toLocaleString()}
                      </span>
                    )}
                    <time dateTime={new Date(group.latestAtMs).toISOString()} className="shrink-0 text-ui-11.5 text-dim">
                      {time(group.latestAtMs)}
                    </time>
                  </div>
                  <div className="mt-1 ml-3.5 flex min-w-0 items-center gap-1.5" title={`${group.representative.connectionName} / ${group.database}`}>
                    {(!activeDb || !sameDb(activeDb, { connectionId: group.connectionId, database: group.database })) && (
                      <span className="max-w-[45%] shrink-0 truncate rounded bg-badge-bg px-1 text-ui-11.5 text-badge">
                        {group.representative.connectionName} / {group.database}
                      </span>
                    )}
                    <span className="min-w-0 truncate font-mono text-code-sm text-dim" title={group.excerpt}>
                      {group.excerpt || group.title}
                    </span>
                  </div>
                </button>
              </Fragment>
            );
          })}
          {list?.hasMore && (
            <Button
              data-testid="history-load-older"
              variant="ghost"
              className="my-2 w-full justify-center"
              disabled={history.loadingOlder}
              onClick={history.loadOlder}
            >
              {history.loadingOlder ? "Loading…" : "Load older queries"}
            </Button>
          )}
        </div>
        <section aria-label="Query preview" data-testid="history-preview" className="flex min-h-0 min-w-0 flex-col gap-4 p-6">
          {!selected ? (
            <div className="grid flex-1 place-items-center text-ui-13 text-dim">Select a query to preview</div>
          ) : (
            <>
              <div className="min-w-0 shrink-0">
                <h2 className="truncate text-ui-14 font-semibold text-fg" title={selected.title}>
                  {selected.title}
                </h2>
                <div className="mt-2 flex min-w-0 items-center gap-2 text-ui-11.5 text-dim">
                  <OutcomeDot outcome={selected.representative.outcome} />
                  <span
                    className={cx("min-w-0 truncate", selected.representative.outcome.kind === "Error" && "text-danger")}
                    title={outcomeText(selected.representative.outcome)}
                  >
                    {outcomeText(selected.representative.outcome)}
                  </span>
                  <span className="shrink-0">· {formatElapsed(selected.representative.elapsedMs)}</span>
                </div>
                <div className="mt-1.5 flex min-w-0 items-center gap-1.5 text-ui-11 text-dim">
                  <DatabaseBadge databaseKind={selected.databaseKind} />
                  <span className="truncate" title={`${selected.representative.connectionName} / ${selected.database}`}>
                    {selected.representative.connectionName} / {selected.database}
                  </span>
                  <span className="shrink-0">
                    · {day(selected.representative.atMs, now)} {time(selected.representative.atMs)}
                  </span>
                </div>
              </div>
              <div className="relative min-h-0 flex-1 overflow-hidden rounded-lg border border-control bg-bg">
                {detail ? (
                  <SqlText mode="view" text={detail.sql} databaseKind={detail.databaseKind} label="Query SQL" testId="history-sql" />
                ) : (
                  <p role="status" className="p-4 text-ui-12 text-dim">
                    Loading SQL…
                  </p>
                )}
                <button
                  type="button"
                  data-testid="history-copy"
                  disabled={!detail}
                  onClick={() => void copy()}
                  className="absolute top-2 right-2 flex h-6 items-center gap-1.5 rounded-md bg-raised px-2 text-ui-11 text-muted shadow-sm outline-none hover:text-fg focus-visible:ring-1 focus-visible:ring-focus disabled:opacity-40"
                >
                  {copied ? <CheckIcon size={13} /> : <CopyIcon size={13} />}
                  {copied ? "Copied" : "Copy"}
                  <Kbd className="text-dim">{mod}C</Kbd>
                </button>
              </div>
              <span className="sr-only" role="status">
                {copied ? "Query copied" : ""}
              </span>
              {copyError && (
                <p role="alert" className="text-ui-11 text-danger">
                  Could not copy. Select the SQL and copy it manually.
                </p>
              )}
              <div className="shrink-0">
                <div className="mb-2 text-ui-11.5 text-dim">
                  Ran {selected.runCount.toLocaleString()} {selected.runCount === 1 ? "time" : "times"}
                  {selected.failedCount > 0 && <span className="ml-2 text-danger">{selected.failedCount.toLocaleString()} failed</span>}
                </div>
                <div data-testid="history-timeline" className="max-h-[130px] overflow-x-hidden overflow-y-auto">
                  {detail?.runs.map((run) => (
                    <div key={run.id} data-testid="history-run" className="flex min-w-0 items-center gap-2 py-1 text-ui-11.5">
                      <time className="w-28 shrink-0 text-muted" title={new Date(run.atMs).toLocaleString()}>
                        {day(run.atMs, now)} {time(run.atMs)}
                      </time>
                      <OutcomeDot outcome={run.outcome} />
                      <span
                        className={cx("w-24 shrink-0 truncate", run.outcome.kind === "Error" ? "text-danger" : "text-dim")}
                        title={outcomeText(run.outcome)}
                      >
                        {outcomeText(run.outcome)}
                      </span>
                      <div className="h-1 min-w-3 flex-1 overflow-hidden rounded-full bg-control">
                        <div
                          className={cx(
                            "h-full rounded-full",
                            run.outcome.kind === "Error" ? "bg-danger" : run.outcome.kind === "Cancelled" ? "bg-faint" : "bg-keyword",
                          )}
                          style={{ width: `${Math.max(2, (100 * run.elapsedMs) / Math.max(1, detail.maxElapsedMs))}%` }}
                        />
                      </div>
                      <span className="w-14 shrink-0 text-right text-dim tabular-nums">{formatElapsed(run.elapsedMs)}</span>
                    </div>
                  ))}
                  {detail?.hasMore && (
                    <Button data-testid="history-load-runs" variant="ghost" disabled={history.loadingRuns} onClick={history.loadRuns}>
                      {history.loadingRuns ? "Loading…" : "Load older runs"}
                    </Button>
                  )}
                </div>
              </div>
              <div className="flex shrink-0 flex-wrap items-center gap-2">
                <Button variant="primary" data-testid="history-open" disabled={!detail} onClick={() => act("open")}>
                  Open in new tab<Kbd tone="accent">↵</Kbd>
                </Button>
                <Button data-testid="history-run-again" disabled={!detail} onClick={() => act("run")}>
                  <PlayIcon fill="currentColor" size={12} className="text-green" />
                  Run again<Kbd className="text-dim">{mod}↵</Kbd>
                </Button>
                <Button
                  variant="ghost"
                  data-testid="history-insert"
                  disabled={!detail || !insertEnabled}
                  title={insertEnabled ? "Insert into the active tab" : "Open a tab to insert this query"}
                  onClick={() => act("insert")}
                >
                  Insert at cursor<Kbd className="text-dim">Alt+↵</Kbd>
                </Button>
                <Button
                  variant="ghost"
                  data-testid="history-save"
                  disabled={!detail}
                  title="Keep this query in Saved queries"
                  onClick={() => detail && void saveHistoryQuery(detail.sql, detail.databaseKind)}
                >
                  <BookmarkPlusIcon size={14} />
                  Save query
                </Button>
              </div>
            </>
          )}
        </section>
      </div>
      <div className="flex min-h-10 shrink-0 items-center gap-3 border-t border-control px-4 text-ui-11.5 text-dim">
        <span>
          <Kbd>↑↓</Kbd> Navigate
        </span>
        <span>
          <Kbd>↵</Kbd> Open
        </span>
        <span>
          <Kbd>{mod}↵</Kbd> Run
        </span>
        <span>
          <Kbd>esc</Kbd> Close
        </span>
        <span data-testid="history-count" role="status" className="ml-auto shrink-0">
          {count === null ? "…" : count.toLocaleString()} {count === 1 ? "query" : "queries"}
        </span>
        <button
          type="button"
          data-testid="history-clear"
          onClick={() => void clearHistory()}
          disabled={!list}
          className="shrink-0 rounded px-1 py-1 text-dim outline-none hover:text-danger focus-visible:ring-1 focus-visible:ring-focus disabled:opacity-40"
        >
          Clear history…
        </button>
      </div>
    </div>
  );
}

export function HistoryPalette() {
  const open = useWorkspace((s) => s.overlay.kind === "history");
  const close = useWorkspace((s) => s.close);
  const restoreFocus = useRef(false);
  return (
    <Dialog.Root open={open} onOpenChange={(value) => !value && close()}>
      <Dialog.Portal>
        <DialogSurface
          aria-describedby={undefined}
          data-testid="history-palette"
          onCloseAutoFocus={(event) => {
            if (restoreFocus.current) {
              event.preventDefault();
              restoreFocus.current = false;
              focusEditor();
            }
          }}
          className="h-[640px] w-[960px] overflow-hidden"
        >
          <Dialog.Title className="sr-only">Query history</Dialog.Title>
          <HistoryBrowser
            close={close}
            restoreEditorFocus={() => {
              restoreFocus.current = true;
            }}
          />
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
