import { DENSITY_FACTOR, gridRowHeight } from "../../shell/appearance";
import { StartCommands } from "../../shell/StartCommands";
import { EditorView } from "@codemirror/view";
import { ChevronRightIcon, CircleAlertIcon, CopyPlusIcon, ListIndentIncreaseIcon, PlayIcon, PlusIcon, SquareIcon, XIcon } from "lucide-react";
import { useEffect, useEffectEvent, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { flushSync } from "react-dom";
import type { RunState, TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { canCancel, hasUncommittedWork, isBusy, resultKey, rowResults, runSummary, shownResult, type ResultPick } from "../../state/status";
import { notebookOf } from "../../state/notebooks";
import { activeId, shownNotebook, useWorkspace } from "../../state/workspace";
import { cx, IconButton } from "../../ui/primitives";
import { useNow } from "../../ui/useNow";
import { hasResultPager } from "../results/format";
import { ResultsPane } from "../results/ResultsPane";
import { CellGrip } from "./CellGrip";
import { CellResizeHandle } from "./CellResizeHandle";
import { CELL_SCROLL_MARGIN, cellHeights, EDITOR_MIN, pinnedTop, type DraggedHeights } from "./cellSizing";
import { closeTab } from "./closeTab";
import { goToCell } from "../notebooks/cellNavigation";
import { staleness, stalenessText } from "../notebooks/references";
import { docText } from "./docText";
import { activateEditor, docOf, onBuffersChanged, textOf } from "./editorStates";
import { cancelActive, formatActive, openSqlTab, runActive } from "./runActions";
import { SqlEditor } from "./SqlEditor";
import { SqlText } from "./SqlText";
import { databaseKindFor } from "../../state/databaseKind";

const MOVE_KEYS: Partial<Record<string, -1 | 1>> = { ArrowUp: -1, ArrowDown: 1 };

function AddCell({ after }: { after?: TabId }) {
  return (
    <div className="notebook-add">
      <IconButton aria-label="Add SQL cell" data-testid="add-cell" onClick={() => openSqlTab({ after })} title="Add SQL cell">
        <PlusIcon size={14} />
      </IconButton>
    </div>
  );
}

const onStaleInputs = (changed: () => void) => {
  const buffers = onBuffersChanged(changed);
  const workspace = useWorkspace.subscribe(changed);
  return () => {
    buffers();
    workspace();
  };
};

/** Why the cell's result no longer matches the cells it references, or "" while it does. */
function staleReason(id: TabId): string {
  const s = useWorkspace.getState();
  const sources = s.runSources[id];
  if (!sources?.length) return "";
  const cells = (notebookOf(s.notebooks, id)?.cells ?? []).flatMap((c) => {
    const doc = docOf(c);
    const tab = s.tabs[c];
    return tab && doc ? [{ name: tab.title, text: docText(doc) }] : [];
  });
  return stalenessText(staleness(sources, cells));
}

const UNDRAGGED: DraggedHeights = { editor: null, results: null };

/** A collapsed cell's last run, as the results pane would sum it up, so a failure shows without opening the cell. */
function RunStatus({ id, run, picked }: { id: TabId; run: RunState; picked: ResultPick | undefined }) {
  const now = useNow(run.kind === "Running", 1000);
  const summary = runSummary(run, picked, now);
  if (!summary) return null;
  return (
    <span
      data-testid={`cell-run-status-${id}`}
      title={summary.text}
      className={cx("flex max-w-[40%] shrink-0 items-center gap-1 text-ui-11 font-normal tabular-nums", summary.tone === "error" ? "text-danger" : "text-dim")}
    >
      {summary.tone === "error" && <CircleAlertIcon size={12} className="shrink-0" aria-label="Query failed" />}
      <span className="truncate">{summary.text}</span>
    </span>
  );
}

function NotebookCell({ id, index, viewport, focused }: { id: TabId; index: number; viewport: number; focused: boolean }) {
  const tab = useWorkspace((s) => s.tabs[id]);
  const active = useWorkspace((s) => activeId(s) === id);
  const codeSize = useWorkspace((s) => s.settings.codeFont.size);
  const density = useWorkspace((s) => s.settings.density);
  const pageSize = useWorkspace((s) => s.settings.tablePageSize);
  const databaseKind = useWorkspace((s) => databaseKindFor(s.tabs[id]?.console.db ?? null, s.configs));
  const console = useLive((s) => s.consoles[id]);
  const text = useSyncExternalStore(
    onBuffersChanged,
    () => textOf(id),
    () => textOf(id),
  );
  const stale = useSyncExternalStore(
    onStaleInputs,
    () => staleReason(id),
    () => staleReason(id),
  );
  const cell = useRef<HTMLElement>(null);
  const [focusOnActivate, setFocusOnActivate] = useState(active);
  const [renaming, setRenaming] = useState(false);
  const [draftTitle, setDraftTitle] = useState("");
  const [dragged, setDragged] = useState<DraggedHeights>(UNDRAGGED);
  const hasResults = (tab?.table != null && active) || (console !== undefined && console.run.kind !== "Idle");
  const rowCounts = console ? rowResults(console.run).map((result) => result.meta.rowCount) : [];
  const largest = rowCounts.length > 0 ? Math.max(...rowCounts) : undefined;
  const table = tab?.table != null;
  const picked = useWorkspace((s) => s.activeResult[id]);
  const shown = console ? shownResult(console.run, picked) : undefined;
  const shownKey = shown ? resultKey(shown) : "";
  const chart = useWorkspace((s) => s.resultDisplay[id]?.[shownKey]?.display === "chart");
  const collapsed = tab?.collapsed === true;
  const sizing = {
    viewport,
    lines: text.split("\n").length,
    lineHeight: Math.round(codeSize * 1.54),
    results: hasResults
      ? {
          table,
          rows: largest,
          pageSize,
          rowHeight: gridRowHeight(codeSize, density),
          chrome: Math.round(80 * DENSITY_FACTOR[density]),
          pager: !table && largest !== undefined && hasResultPager(largest),
          chart,
        }
      : null,
    focused,
  };
  const heights = cellHeights({ ...sizing, dragged });
  // A size the user chose never scrolls the notebook, so only the automatic size keeps the cell in view.
  const automaticResults = cellHeights({ ...sizing, dragged: UNDRAGGED }).results;
  const wasFocused = useRef(focused);
  useEffect(() => {
    const refocused = wasFocused.current !== focused;
    wasFocused.current = focused;
    // The notebook pins a newly focused cell while it grows, so this keeps it in view only through later size changes.
    if (focused && !refocused) cell.current?.scrollIntoView({ block: "nearest" });
  }, [focused, automaticResults, viewport, collapsed, index]);
  if (!tab) return null;
  const activate = () => {
    setFocusOnActivate(false);
    useWorkspace.getState().setActive(id);
    activateEditor(id);
  };
  const firstLine = text.split("\n").find((line) => line.trim()) ?? "";
  const busy = isBusy(console);
  const pending = hasUncommittedWork(console);
  const cancellable = canCancel(console);
  const editorHandle = {
    label: `Resize query in ${tab.title}`,
    testId: "cell-editor-resize",
    value: heights.editor,
    min: EDITOR_MIN,
    max: heights.editorMax,
    onResize: (px: number | null) => setDragged((current) => ({ ...current, editor: px })),
  };
  const lowerHandle =
    heights.results === null
      ? editorHandle
      : {
          label: `Resize results in ${tab.title}`,
          testId: "cell-results-resize",
          value: heights.results,
          min: heights.resultsMin,
          max: heights.resultsMax,
          onResize: (px: number | null) => setDragged((current) => ({ ...current, results: px })),
        };
  return (
    <>
      <section
        ref={cell}
        data-testid={`notebook-cell-${id}`}
        data-active={active}
        data-collapsed={collapsed}
        style={{ scrollMarginBlock: CELL_SCROLL_MARGIN }}
        className={cx("notebook-cell", stale && "notebook-cell-stale", active && "notebook-cell-active")}
        onFocusCapture={() => {
          if (!active) activate();
        }}
      >
        <div className="notebook-cell-heading">
          <IconButton
            aria-label={collapsed ? "Expand cell" : "Collapse cell"}
            aria-expanded={!collapsed}
            title={collapsed ? "Expand cell" : "Collapse cell"}
            className="-mr-1"
            onClick={() => useWorkspace.getState().setCollapsed([id], !collapsed)}
          >
            <ChevronRightIcon size={14} className={cx("transition-transform", !collapsed && "rotate-90")} />
          </IconButton>
          <CellGrip id={id} index={index} title={tab.title} />
          {renaming ? (
            <input
              autoFocus
              aria-label="Cell title"
              className="min-w-0 flex-1 rounded border border-control bg-bg px-1 text-ui-12"
              value={draftTitle}
              onChange={(e) => setDraftTitle(e.target.value)}
              onBlur={() => {
                if (draftTitle.trim()) useWorkspace.getState().renameTab(id, draftTitle.trim());
                setRenaming(false);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") setRenaming(false);
              }}
            />
          ) : (
            <button
              type="button"
              data-testid={`tab-${id}`}
              data-tab-id={id}
              data-active={active}
              onClick={() => activate()}
              onKeyDown={(e) => {
                const move = e.altKey ? MOVE_KEYS[e.key] : undefined;
                if (move) {
                  e.preventDefault();
                  const title = e.currentTarget;
                  // React moves a cell by reinserting its nodes, which blurs the title in an engine that drops focus on removal.
                  flushSync(() => useWorkspace.getState().moveCell(id, index + move));
                  title.focus();
                  return;
                }
                const direction = ["ArrowLeft", "ArrowUp"].includes(e.key) ? -1 : ["ArrowRight", "ArrowDown"].includes(e.key) ? 1 : 0;
                if (!direction && e.key !== "Home" && e.key !== "End") return;
                e.preventDefault();
                const titles = [...(cell.current?.parentElement?.querySelectorAll<HTMLButtonElement>("button[data-tab-id]") ?? [])];
                const at = titles.indexOf(e.currentTarget);
                const next = e.key === "Home" ? titles[0] : e.key === "End" ? titles.at(-1) : titles[(at + direction + titles.length) % titles.length];
                next?.focus();
              }}
              onDoubleClick={() => {
                setDraftTitle(tab.title);
                setRenaming(true);
              }}
              className="flex min-w-0 flex-1 items-center gap-2 text-left text-ui-12 font-medium text-fg"
              title={tab.title}
            >
              <span className="min-w-0 truncate">{tab.title}</span>
              {collapsed && (
                <>
                  <span data-testid={`cell-summary-${id}`} className="min-w-0 flex-1 truncate font-mono text-ui-11 font-normal text-dim">
                    {firstLine.trim()}
                  </span>
                  {console && <RunStatus id={id} run={console.run} picked={picked} />}
                </>
              )}
              {stale && (
                <span
                  data-testid={`stale-${id}`}
                  className="shrink-0 rounded bg-amber/12 px-1.5 py-px text-ui-10 font-medium text-amber"
                  title={`${stale} Run this cell to update it.`}
                >
                  Stale
                </span>
              )}
            </button>
          )}
          {pending && (
            <span
              className={cx("size-1.5 shrink-0 rounded-full", console?.txn === "Aborted" ? "bg-danger" : "bg-amber")}
              role="img"
              aria-label={console?.txn === "Aborted" ? "Transaction aborted" : "Uncommitted transaction"}
            />
          )}
          <IconButton
            aria-label="Format SQL"
            title="Format SQL"
            onClick={() => {
              activate();
              void formatActive();
            }}
          >
            <ListIndentIncreaseIcon size={14} />
          </IconButton>
          <IconButton
            data-testid={active ? (cancellable ? "cancel-btn" : "run-btn") : undefined}
            aria-label={cancellable ? "Stop" : "Run cell"}
            title={cancellable ? "Stop" : "Run cell"}
            disabled={busy && !cancellable}
            onClick={() => {
              activate();
              if (cancellable) cancelActive();
              else runActive({ kind: "all" });
            }}
          >
            {cancellable ? <SquareIcon fill="currentColor" size={11} /> : <PlayIcon fill="currentColor" size={13} className="text-run-icon" />}
          </IconButton>
          <IconButton
            aria-label="Duplicate cell"
            title="Duplicate cell"
            onClick={() => openSqlTab({ db: tab.console.db, schema: tab.console.schema, text, title: `${tab.title} copy` })}
          >
            <CopyPlusIcon size={14} />
          </IconButton>
          <IconButton aria-label="Close cell" title="Close cell" onClick={() => void closeTab(id)}>
            <XIcon size={14} />
          </IconButton>
        </div>
        {!collapsed && (
          <div
            className="notebook-editor"
            style={{ height: heights.editor }}
            onMouseDownCapture={(event) => {
              if (active || event.button !== 0) return;
              // The preview would take focus itself, then lose it as the shared editor replaces it.
              event.preventDefault();
              const pos = EditorView.findFromDOM(event.currentTarget)?.posAtCoords({ x: event.clientX, y: event.clientY }, false);
              goToCell(id, pos === undefined ? undefined : () => pos);
            }}
          >
            {active ? (
              <SqlEditor tabId={id} autoFocus={focusOnActivate} />
            ) : (
              <SqlText mode="view" wrap={false} text={text} databaseKind={databaseKind} label={`SQL in ${tab.title}`} testId={`cell-preview-${id}`} />
            )}
          </div>
        )}
        {!collapsed && heights.results !== null && (
          <>
            <CellResizeHandle {...editorHandle} lineColor="bg-border" />
            <div
              className="notebook-results"
              style={{ height: heights.results }}
              onMouseDown={() => {
                if (!active) activate();
              }}
            >
              <ResultsPane tabId={id} />
            </div>
          </>
        )}
      </section>
      {!collapsed && <CellResizeHandle {...lowerHandle} lineColor="bg-transparent" />}
      <AddCell after={id} />
    </>
  );
}

function useFocusAfterClick(willFocus: (id: TabId) => void) {
  const active = useWorkspace(activeId);
  const [focused, setFocused] = useState(active);
  const pressed = useRef(false);
  const focus = useEffectEvent((next: TabId | null) => {
    if (next === focused) return;
    if (next) willFocus(next);
    setFocused(next);
  });
  useLayoutEffect(() => {
    if (!pressed.current) focus(active);
  }, [active]);
  useEffect(() => {
    const release = () => {
      if (!pressed.current) return;
      pressed.current = false;
      setTimeout(() => focus(activeId(useWorkspace.getState())), 0);
    };
    window.addEventListener("pointerup", release, true);
    window.addEventListener("pointercancel", release, true);
    return () => {
      window.removeEventListener("pointerup", release, true);
      window.removeEventListener("pointercancel", release, true);
    };
  }, []);
  const onPointerDownCapture = () => {
    pressed.current = true;
  };
  return { focused, onPointerDownCapture };
}

const NO_CELLS: TabId[] = [];
const SCROLL_KEYS = ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"];
const cellRect = (scroller: HTMLElement, id: TabId) => scroller.querySelector(`[data-testid="notebook-cell-${id}"]`)?.getBoundingClientRect();
const resizing = (animation: Animation) => animation instanceof CSSTransition && animation.transitionProperty === "height";

export function NotebookWorkspace() {
  const cells = useWorkspace((s) => shownNotebook(s)?.cells ?? NO_CELLS);
  const scroller = useRef<HTMLDivElement>(null);
  const settle = useRef<{ id: TabId; anchorTop: number } | null>(null);
  const { focused, onPointerDownCapture } = useFocusAfterClick((id) => {
    const el = scroller.current;
    const rect = el && cellRect(el, id);
    if (!el || !rect) return;
    settle.current = { id, anchorTop: rect.top - el.getBoundingClientRect().top };
    el.dataset.settling = "";
  });
  const [viewport, setViewport] = useState(0);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const measure = () => setViewport(el.clientHeight);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    let frame = 0;
    const step = () => {
      const pin = settle.current;
      const rect = pin && cellRect(el, pin.id);
      if (pin && rect) el.scrollTop += rect.top - el.getBoundingClientRect().top - pinnedTop(pin.anchorTop, rect.height, el.clientHeight, CELL_SCROLL_MARGIN);
      if (el.getAnimations({ subtree: true }).some(resizing)) frame = requestAnimationFrame(step);
      else {
        settle.current = null;
        delete el.dataset.settling;
      }
    };
    const release = (e: Event) => {
      if (!(e instanceof KeyboardEvent) || SCROLL_KEYS.includes(e.key)) settle.current = null;
    };
    step();
    const inputs = ["wheel", "touchmove", "keydown"] as const;
    for (const input of inputs) el.addEventListener(input, release, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      for (const input of inputs) el.removeEventListener(input, release);
    };
  }, [focused]);
  return (
    <div ref={scroller} data-testid="notebook" className="notebook-workspace" onPointerDownCapture={onPointerDownCapture}>
      {cells.length === 0 ? (
        <StartCommands />
      ) : (
        cells.map((id, index) => <NotebookCell key={id} id={id} index={index} viewport={viewport} focused={focused === id} />)
      )}
    </div>
  );
}
