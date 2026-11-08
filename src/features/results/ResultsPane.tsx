import * as Menu from "@radix-ui/react-dropdown-menu";
import {
  ChartColumnIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronFirstIcon,
  ChevronLastIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  MinusIcon,
  PlusIcon,
  SquareTerminalIcon,
  Table2Icon,
  Undo2Icon,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type RefObject } from "react";
import type { ConsoleState, QueryError, RunState, TabId } from "../../ipc/bindings";
import { useLive, type OutputLog } from "../../state/live";
import {
  isBusy,
  outputTone,
  resultKey,
  resultView,
  rowResults,
  runStatements,
  runSummary,
  shownResult,
  type ResultDisplay,
  type ResultView,
  type Summary,
} from "../../state/status";
import { useCatalog, viewFor } from "../../state/catalog";
import { databaseKindFor } from "../../state/databaseKind";
import { NO_EDITS, pendingCount, useTableEdits } from "../../state/tableEdits";
import { isFiltered, isTableTab, useWorkspace, type ResultDisplayPref, type TableTab } from "../../state/workspace";
import { Button, cx, IconButton, MenuContent, MenuItem, Spinner } from "../../ui/primitives";
import { isMac } from "../../shell/hostedWindow";
import { useNow } from "../../ui/useNow";
import {
  isBrowsing,
  lastTablePage,
  loadTableOnce,
  onEditorDocChanged,
  revertEdits,
  saveSettings,
  setTablePage,
  submitEdits,
  updateTableQuery,
} from "../editor/runActions";
import { defaultChart, type ChartSpec } from "./chart";
import { ChartView } from "./ChartView";
import { ExportMenu } from "./ExportMenu";
import { FilterBar } from "./FilterBar";
import { groupDigits, hasResultPager, PAGE_SIZES, pageStatus, pluralRows, resultPage, type RowWindow } from "./format";
import { ErrorBlock, OutputPanel } from "./OutputPanel";
import { PlanView } from "./PlanView";
import { gridEditing, ResultGrid, type GridActions, type GridEditing, type GridQuery } from "./ResultGrid";
import { acceptSelectionTotals, type SelectionSlot } from "./selectionTotals";
import { cycleTableOrder, filterForValue, useFilterDrafts, useTableOrder } from "./tableQuery";

const NO_OUTPUT: OutputLog = { entries: [], dropped: 0 };

function PagerBar({
  testId,
  pageTestId,
  page,
  label,
  isLast,
  lastDisabled,
  onPage,
  onLast,
}: {
  testId: string;
  pageTestId: string;
  page: number;
  label: string;
  isLast: boolean;
  lastDisabled: boolean;
  onPage: (page: number) => void;
  onLast: () => void;
}) {
  const size = useWorkspace((s) => s.settings.tablePageSize);
  return (
    <div data-testid={testId} className="flex h-9 shrink-0 items-center justify-center gap-0.5 border-t border-grid bg-bg px-2 text-ui-12.5 text-muted">
      <IconButton aria-label="First page" disabled={page === 0} onClick={() => onPage(0)}>
        <ChevronFirstIcon size={15} />
      </IconButton>
      <IconButton aria-label="Previous page" disabled={page === 0} onClick={() => onPage(page - 1)}>
        <ChevronLeftIcon size={15} />
      </IconButton>
      <Menu.Root modal={false}>
        <Menu.Trigger
          data-testid={pageTestId}
          className="inline-flex h-7 items-center gap-1 rounded-md pr-1.5 pl-2 whitespace-nowrap text-fg tabular-nums hover:bg-hover data-[state=open]:bg-hover"
        >
          {label}
          {/* The name keeps the visible range first, so a voice command that says what it sees still finds it. */}
          <span className="sr-only">, rows per page</span>
          <ChevronDownIcon size={13} className="text-muted" />
        </Menu.Trigger>
        <MenuContent align="end">
          {PAGE_SIZES.map((n) => (
            <MenuItem key={n} testId={`page-size-${n}`} checked={n === size} onSelect={() => void saveSettings({ tablePageSize: n })}>
              {groupDigits(String(n))} per page
            </MenuItem>
          ))}
        </MenuContent>
      </Menu.Root>
      <IconButton aria-label="Next page" disabled={isLast} onClick={() => onPage(page + 1)}>
        <ChevronRightIcon size={15} />
      </IconButton>
      <IconButton aria-label="Last page" disabled={lastDisabled} onClick={onLast}>
        <ChevronLastIcon size={15} />
      </IconButton>
    </div>
  );
}

function TablePager({ tab, rows }: { tab: TableTab; rows: number }) {
  const size = useWorkspace((s) => s.settings.tablePageSize);
  const busy = useLive((s) => isBusy(s.consoles[tab.id]));
  const { schema, table, page, query, exactRowCount } = tab.table;
  const estimate = useCatalog(
    (s) =>
      viewFor(s.views, tab.console.db)
        ?.catalog.schemas.find((sc) => sc.name === schema)
        ?.relations.find((r) => r.name === table)?.rowEstimate ?? null,
  );
  const { label, isLast } = pageStatus(page, size, rows, isFiltered(query) ? null : estimate, exactRowCount);
  return (
    <PagerBar
      testId="table-pager"
      pageTestId="table-page"
      page={page}
      label={label}
      isLast={isLast}
      lastDisabled={isLast || busy}
      onPage={(next) => setTablePage(tab.id, next)}
      onLast={() => void lastTablePage(tab.id)}
    />
  );
}

function SummaryText({ summary }: { summary: Summary }) {
  return (
    <span
      data-testid="result-status"
      data-tone={summary.tone}
      className={cx("flex min-w-0 items-center gap-1.5 text-ui-13", summary.tone === "error" ? "text-danger" : "text-dim")}
    >
      {summary.tone === "ok" && <CheckIcon size={15} className="shrink-0 text-green" />}
      {summary.tone === "error" && <CircleAlertIcon size={14} className="shrink-0" />}
      {summary.tone === "running" && <Spinner />}
      <span title={summary.text} className="line-clamp-2 [overflow-wrap:anywhere]">
        {summary.text}
      </span>
    </span>
  );
}

/**
 * Says the settled summary to a screen reader. It stays empty while the run ticks, so the clock is not read out and
 * a repeat run with the same summary is heard again. The pane keys it by tab, so switching tabs reads nothing.
 */
function RunAnnouncement({ summary }: { summary: Summary | null }) {
  const text = !summary || summary.tone === "running" ? "" : summary.tone === "error" ? `Query failed: ${summary.text}` : summary.text;
  return (
    <span data-testid="run-announcement" role="status" className="sr-only">
      {text}
    </span>
  );
}

function CursorPosition() {
  const { line, col } = useWorkspace((s) => s.cursor);
  return (
    <span data-testid="cursor-position" className="text-ui-12.5 whitespace-nowrap text-dim tabular-nums">
      Ln {line}, Col {col}
    </span>
  );
}

function ExportStatus({ progress }: { progress: NonNullable<ConsoleState["export"]> }) {
  return (
    <span data-testid="export-status" role="status" className="flex min-w-0 items-center gap-1.5 text-ui-13 text-dim">
      <Spinner />
      <span className="line-clamp-2 tabular-nums [overflow-wrap:anywhere]">Exporting {pluralRows(progress.rows)}…</span>
    </span>
  );
}

function ErrorPanel({ error }: { error: QueryError }) {
  return (
    <div className="selectable flex-1 overflow-auto bg-bg px-5 py-4 text-ui-13 leading-6">
      <ErrorBlock error={error} />
    </div>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className="grid flex-1 place-items-center bg-bg text-ui-13 text-dim">{children}</div>;
}

function EditBar({
  tabId,
  editable,
  canSubmit,
  actions,
}: {
  tabId: TabId | null;
  editable: boolean;
  canSubmit: boolean;
  actions: RefObject<GridActions | null>;
}) {
  const edits = useTableEdits((s) => (tabId ? s.byTab[tabId] : undefined) ?? NO_EDITS);
  const pending = pendingCount(edits);
  const busy = useTableEdits((s) => (tabId ? s.submitting[tabId] === true : false));
  if (!editable && pending === 0) return null;
  return (
    <div data-testid="edit-bar" className="flex max-w-full flex-wrap items-center gap-0.5">
      <IconButton data-testid="add-row" aria-label="Add row" title="Add row" disabled={!editable} onClick={() => actions.current?.addRow()}>
        <PlusIcon size={16} />
      </IconButton>
      <IconButton
        data-testid="delete-rows"
        aria-label="Delete selected rows"
        title="Delete or restore the selected rows (⌘⌫)"
        disabled={!editable}
        onClick={() => actions.current?.deleteSelectedRows()}
      >
        <MinusIcon size={16} />
      </IconButton>
      {pending > 0 && (
        <>
          <div className="mx-1.5 h-4 w-px bg-control" />
          <span data-testid="pending-count" className="mr-1.5 text-ui-12.5 whitespace-nowrap text-amber tabular-nums">
            {pending} pending
          </span>
          <Button
            data-testid="revert-edits"
            variant="ghost"
            className="h-7 px-2"
            title="Discard the staged changes"
            disabled={busy}
            onClick={() => tabId && void revertEdits(tabId)}
          >
            <Undo2Icon size={14} />
            Revert
          </Button>
          <Button
            data-testid="submit-edits"
            variant="primary"
            className="h-7 px-2.5"
            title={canSubmit ? "Send the staged changes (⌘S)" : "SQL Server staged edits are unavailable. Use SQL scripts."}
            disabled={!canSubmit || busy}
            onClick={() => tabId && void submitEdits(tabId)}
          >
            <CheckIcon size={14} />
            Submit
          </Button>
        </>
      )}
    </div>
  );
}

function Segment({ testId, pressed, onClick, children }: { testId: string; pressed: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      data-testid={testId}
      aria-pressed={pressed}
      onClick={onClick}
      className={cx(
        "inline-flex h-6 items-center gap-1 rounded-[5px] border px-2 text-ui-12 transition-colors",
        pressed ? "border-control bg-raised text-fg shadow-[0_1px_2px_rgba(0,0,0,0.08)]" : "border-transparent text-dim hover:text-fg",
      )}
    >
      {children}
    </button>
  );
}

function failedError(run: RunState): QueryError | undefined {
  if (run.kind !== "Finished") return undefined;
  const failed = run.statements.find((s) => s.status.kind === "Failed");
  return failed?.status.kind === "Failed" ? failed.status.error : undefined;
}

function RunTail({ run }: { run: RunState }) {
  if (run.kind === "Idle")
    return (
      <div className="px-4 text-center">
        <p className="text-ui-13.5 text-muted">Results appear here</p>
        <p className="mt-1">Run a statement with {isMac(navigator.userAgent) ? "⌘↵" : "Ctrl+Enter"}.</p>
      </div>
    );
  if (run.kind === "Running") {
    return (
      <span className="flex items-center gap-2">
        <Spinner /> Running
      </span>
    );
  }
  const error = failedError(run);
  if (error) return <ErrorBlock error={error} />;
  if (run.outcome === "Cancelled") return "Cancelled.";
  const commands = runStatements(run).flatMap((s) => s.outputs.flatMap((o) => (o.kind === "Command" ? [o.tag] : [])));
  if (commands.length) return commands.join(", ");
  return rowResults(run).length ? null : "The statement returned no rows.";
}

function ResultBody({
  run,
  view,
  output,
  query,
  editing,
  actions,
  onSelectionTotals,
  rowWindow,
  onFindOutsideWindow,
  chart,
  onChart,
}: {
  run: RunState;
  view: ResultView;
  output: OutputLog;
  query: GridQuery | null;
  editing: GridEditing | null;
  actions: RefObject<GridActions | null>;
  onSelectionTotals: (slot: SelectionSlot) => void;
  rowWindow: RowWindow | null;
  onFindOutsideWindow: (row: number) => void;
  chart: ChartSpec | null;
  onChart: (spec: ChartSpec) => void;
}) {
  switch (view.kind) {
    case "rows":
      return (
        <ResultGrid
          key={`${view.result.meta.ref.runId}:${view.result.index}`}
          meta={view.result.meta}
          done={view.result.state !== "Streaming"}
          query={query}
          editing={editing}
          actionsRef={actions}
          onSelectionTotals={onSelectionTotals}
          rowWindow={rowWindow}
          onFindOutsideWindow={onFindOutsideWindow}
        />
      );
    case "plan":
      return <PlanView result={view.result} />;
    case "chart":
      return chart ? <ChartView result={view.result} spec={chart} onSpec={onChart} /> : <Empty>No number column to chart.</Empty>;
    case "output":
      return <OutputPanel log={output} />;
    case "tail": {
      const error = failedError(run);
      if (error) return <ErrorPanel error={error} />;
      return (
        <Empty>
          <RunTail run={run} />
        </Empty>
      );
    }
    default: {
      const unreachable: never = view;
      return unreachable;
    }
  }
}

export function ResultsPane({ tabId }: { tabId: TabId | null }) {
  const tab = useWorkspace((s) => (tabId ? s.tabs[tabId] : undefined));
  const picked = useWorkspace((s) => (tabId ? s.activeResult[tabId] : undefined));
  const databaseKind = useWorkspace((s) => databaseKindFor(tab?.console.db ?? null, s.configs));
  const setActiveResult = useWorkspace((s) => s.setActiveResult);
  const console = useLive((s) => (tabId ? s.consoles[tabId] : undefined));
  const run = console?.run ?? { kind: "Idle" as const };
  const busy = isBusy(console);
  const now = useNow(run.kind === "Running", 100);
  const results = rowResults(run);
  const shown = shownResult(run, picked);
  const summary = runSummary(run, picked, now);
  const output = useLive((s) => (tabId ? s.output[tabId] : undefined)) ?? NO_OUTPUT;
  const shownRef = shown ? resultKey(shown) : "";
  const storedPref = useWorkspace((s) => (tabId ? s.resultDisplay[tabId]?.[shownRef] : undefined));
  const pref: ResultDisplayPref = storedPref ?? { key: shownRef, display: "default", chart: null };
  const setDisplayPref = (next: ResultDisplayPref) => {
    if (tabId) useWorkspace.getState().setResultDisplay(tabId, next);
  };
  const chartDefault = shown && !shown.plan ? defaultChart(shown.meta.columns) : null;
  const view = resultView(run, picked, output.entries.length > 0, pref.display);
  const showDisplay = (next: ResultDisplay) => {
    setDisplayPref({ ...pref, display: next });
    if (view.kind === "output" && tabId && shown) setActiveResult(tabId, shown.index);
  };
  const tone = outputTone(output.entries);
  const outputPick = run.kind === "Idle" ? undefined : { kind: "output" as const, runId: run.runId, result: shown?.index };

  const isTable = isTableTab(tab);
  const browsingNow = () => tab !== undefined && isBrowsing(tab);
  const browsing = useSyncExternalStore(onEditorDocChanged, browsingNow, browsingNow);
  const browsingTable = browsing && isTableTab(tab) ? tab : null;
  const pager = browsingTable && view.kind === "rows" ? browsingTable : null;
  const addDraft = useFilterDrafts((s) => s.add);
  const sort = useTableOrder(browsingTable);
  const gridQuery: GridQuery | null = browsingTable && {
    sort,
    filtered: new Set(browsingTable.table.query.filters.map((f) => f.column)),
    onSort: (column, additive) => void cycleTableOrder(browsingTable, column, additive),
    onFilterColumn: (column) => addDraft(browsingTable.id, column),
    onFilterValue: (column, value, negate) =>
      updateTableQuery(browsingTable.id, (q) => ({ ...q, filters: [...q.filters, filterForValue(column, value, negate)] })),
  };
  useEffect(() => {
    if (tabId && isTable) loadTableOnce(tabId);
  }, [tabId, isTable]);
  const catalogView = useCatalog((s) => viewFor(s.views, browsingTable?.console.db));
  const editing = useMemo(
    () =>
      browsingTable && shown && databaseKind === "Postgres"
        ? gridEditing(catalogView, browsingTable.table.schema, browsingTable.table.table, shown.meta)
        : null,
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the deps name the fields the memo reads, not the objects that hold them
    [browsingTable?.console.db, browsingTable?.table.schema, browsingTable?.table.table, catalogView, shown?.meta, databaseKind],
  );
  const actions = useRef<GridActions | null>(null);
  const shownKeyRef = useRef(shownRef);
  shownKeyRef.current = shownRef;
  const [totals, setTotals] = useState<SelectionSlot | null>(null);
  if (view.kind !== "rows" || (totals !== null && totals.key !== shownRef)) {
    if (totals !== null) setTotals(null);
  }
  const onSelectionTotals = (next: SelectionSlot) => {
    setTotals((current) => acceptSelectionTotals(current, next, shownKeyRef.current));
  };
  const selectionText = view.kind === "rows" && totals?.key === shownRef ? totals.text : null;
  const pageSize = useWorkspace((s) => s.settings.tablePageSize);
  const [pageAnchor, setPageAnchor] = useState<{ key: string; row: number } | null>(null);
  if ((pageAnchor?.key ?? "") !== shownRef) setPageAnchor({ key: shownRef, row: 0 });
  const anchorRow = pageAnchor?.key === shownRef ? pageAnchor.row : 0;
  const showRow = (row: number) => setPageAnchor({ key: shownRef, row });
  const rowsPerPage = Math.max(1, pageSize);
  const queryPage =
    view.kind === "rows" && !browsingTable && hasResultPager(view.result.meta.rowCount)
      ? resultPage(view.result.meta.rowCount, rowsPerPage, Math.floor(anchorRow / rowsPerPage))
      : null;
  const exporting = console?.export ?? null;
  const importing = console?.import ?? null;

  return (
    <section className="flex h-full min-h-0 flex-col bg-bg">
      <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-grid py-1.5 pr-2 pl-2.5">
        <EditBar
          tabId={tabId}
          editable={!busy && editing !== null && tabId !== null}
          canSubmit={!busy && databaseKind !== "SqlServer" && shown?.meta.databaseKind !== "SqlServer"}
          actions={actions}
        />
        {results.length > 1 && (
          <div data-testid="result-selector" className="flex max-w-full items-center gap-0.5 overflow-x-auto rounded-md border border-control p-0.5">
            {results.map((r, i) => (
              <button
                key={r.index}
                type="button"
                data-testid={`result-pick-${r.index}`}
                title={`Result ${i + 1} · ${pluralRows(r.meta.rowCount)}`}
                onClick={() => tabId && setActiveResult(tabId, r.index)}
                className={cx(
                  "h-5 min-w-6 shrink-0 rounded px-1.5 font-mono text-ui-11.5",
                  "result" in view && view.result.index === r.index ? "bg-pill text-fg" : "text-dim hover:text-muted",
                )}
              >
                {i + 1}
              </button>
            ))}
          </div>
        )}

        <div className="flex min-w-0 flex-[1_1_12rem] flex-wrap items-center gap-x-3 gap-y-1">
          {importing ? (
            <span data-testid="import-status" role="status" className="flex min-w-0 items-center gap-1.5 text-ui-13 text-dim">
              <Spinner />
              <span className="line-clamp-2 tabular-nums [overflow-wrap:anywhere]">
                {importing.finishing
                  ? "Finishing import…"
                  : `Importing ${importing.rows.toLocaleString()} / ${importing.totalRows.toLocaleString()} rows · ${importing.bytes.toLocaleString()} / ${importing.totalBytes.toLocaleString()} bytes`}
              </span>
            </span>
          ) : exporting ? (
            <ExportStatus progress={exporting} />
          ) : (
            summary && <SummaryText summary={summary} />
          )}
          {selectionText !== null && (
            <span data-testid="result-selection" title={selectionText} className="line-clamp-2 min-w-0 text-ui-13 text-dim [overflow-wrap:anywhere]">
              {selectionText}
            </span>
          )}
        </div>
        <RunAnnouncement key={tabId} summary={summary} />

        <div className="ml-auto flex max-w-full flex-wrap items-center gap-2">
          {tab && <CursorPosition />}
          <div className="flex items-center gap-0.5">
            {shown?.plan && (
              <Button
                data-testid="plan-rows-toggle"
                className="h-7 px-2"
                onClick={() => setDisplayPref({ ...pref, display: pref.display === "rows" ? "default" : "rows" })}
              >
                {pref.display === "rows" ? "Plan" : "Rows"}
              </Button>
            )}
            {chartDefault && (
              <div role="group" aria-label="Result view" className="mr-1 flex h-7 items-center rounded-md bg-hover p-0.5">
                <Segment testId="view-rows" pressed={view.kind === "rows"} onClick={() => showDisplay("rows")}>
                  <Table2Icon size={12} />
                  Rows
                </Segment>
                <Segment testId="view-chart" pressed={view.kind === "chart"} onClick={() => showDisplay("chart")}>
                  <ChartColumnIcon size={12} />
                  Chart
                </Segment>
              </div>
            )}
            <ExportMenu
              tab={tab}
              result={shown}
              table={
                browsingTable && {
                  page: browsingTable.table.page,
                  pageSize,
                  exactRowCount: browsingTable.table.exactRowCount ?? null,
                  filtered: isFiltered(browsingTable.table.query),
                }
              }
              disabled={!shown || busy}
            />
            <IconButton
              data-testid="output-toggle"
              data-tone={tone}
              aria-label="Output"
              title="Output"
              aria-pressed={view.kind === "output"}
              disabled={output.entries.length === 0 || !tabId}
              onClick={() => {
                const pick = view.kind === "output" ? (shown?.index ?? 0) : outputPick;
                if (tabId && pick !== undefined) setActiveResult(tabId, pick);
              }}
              className={cx(view.kind === "output" && "bg-pill!", tone === "warn" ? "text-amber!" : view.kind === "output" && "text-fg!")}
            >
              <SquareTerminalIcon size={16} />
            </IconButton>
          </div>
        </div>
      </div>

      {browsingTable && databaseKind === "SqlServer" && (
        <p data-testid="sqlserver-edit-limit" className="border-b border-grid px-3 py-1 text-ui-11.5 text-dim">
          SQL Server table results are read-only. Use SQL scripts to insert, update or delete rows.
        </p>
      )}
      {exporting?.writeError && (
        <p data-testid="export-write-error" className="border-b border-grid px-3 py-1 text-ui-11.5 text-amber">
          Couldn't write the file: {exporting.writeError}. The query keeps reading to the end so the session survives. Esc stops it now and resets the session.
        </p>
      )}
      {browsingTable && <FilterBar tab={browsingTable} />}

      <div className="relative flex min-h-0 flex-1 flex-col">
        <ResultBody
          run={run}
          view={view}
          output={output}
          query={gridQuery}
          editing={editing}
          actions={actions}
          onSelectionTotals={onSelectionTotals}
          rowWindow={queryPage?.window ?? null}
          onFindOutsideWindow={showRow}
          chart={pref.chart ?? chartDefault}
          onChart={(spec) => setDisplayPref({ ...pref, chart: spec })}
        />
      </div>
      {pager && <TablePager tab={pager} rows={shown?.meta.rowsSeen ?? 0} />}
      {queryPage && (
        <PagerBar
          testId="result-pager"
          pageTestId="result-page"
          page={queryPage.page}
          label={queryPage.label}
          isLast={queryPage.isLast}
          lastDisabled={queryPage.isLast}
          onPage={(page) => showRow(page * rowsPerPage)}
          onLast={() => showRow(queryPage.lastPage * rowsPerPage)}
        />
      )}
    </section>
  );
}
