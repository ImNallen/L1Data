import { DENSITY_FACTOR, gridRowHeight } from "../../shell/appearance";
import { StartCommands } from "../../shell/StartCommands";
import * as Menu from "@radix-ui/react-dropdown-menu";
import { EllipsisIcon, ListIndentIncreaseIcon, PlayIcon, PlusIcon, SquareIcon, XIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import type { TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { canCancel, hasUncommittedWork, isBusy, resultKey, rowResults, shownResult } from "../../state/status";
import { useWorkspace } from "../../state/workspace";
import { Button, cx, IconButton, MenuContent, MenuItem } from "../../ui/primitives";
import { hasResultPager } from "../results/format";
import { ResultsPane } from "../results/ResultsPane";
import { CellResizeHandle } from "./CellResizeHandle";
import { CELL_SCROLL_MARGIN, cellHeights, EDITOR_MIN, type DraggedHeights } from "./cellSizing";
import { closeTab } from "./closeTab";
import { activateEditor, isDirty, onBuffersChanged, textOf } from "./editorStates";
import { cancelActive, explainActive, explainSupported, formatActive, openSqlTab, runActive } from "./runActions";
import { SqlEditor } from "./SqlEditor";
import { SqlText } from "./SqlText";
import { databaseKindFor } from "../../state/databaseKind";

function AddCell({ after }: { after?: TabId }) {
  const add = () => {
    const id = openSqlTab();
    if (!after) return;
    const { order } = useWorkspace.getState();
    const remaining = order.filter((cell) => cell !== id);
    const at = remaining.indexOf(after);
    useWorkspace.setState({ order: [...remaining.slice(0, at + 1), id, ...remaining.slice(at + 1)] });
  };
  return (
    <div className="notebook-add">
      <IconButton aria-label="Add SQL cell" data-testid="add-cell" onClick={add} title="Add SQL cell">
        <PlusIcon size={14} />
      </IconButton>
    </div>
  );
}

function NotebookCell({ id, index, viewport, focused }: { id: TabId; index: number; viewport: number; focused: boolean }) {
  const tab = useWorkspace((s) => s.tabs[id]);
  const active = useWorkspace((s) => s.active === id);
  const showResults = useWorkspace((s) => s.layout.results);
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
  const unsaved = useSyncExternalStore(
    onBuffersChanged,
    () => isDirty(id),
    () => isDirty(id),
  );
  const cell = useRef<HTMLElement>(null);
  const [focusOnActivate, setFocusOnActivate] = useState(active);
  const [renaming, setRenaming] = useState(false);
  const [draftTitle, setDraftTitle] = useState("");
  const [dragged, setDragged] = useState<DraggedHeights>({ editor: null, results: null });
  const [dragging, setDragging] = useState(false);
  const hasResults = showResults && ((tab?.table != null && active) || (console !== undefined && console.run.kind !== "Idle"));
  const rowCounts = console ? rowResults(console.run).map((result) => result.meta.rowCount) : [];
  const largest = rowCounts.length > 0 ? Math.max(...rowCounts) : undefined;
  const table = tab?.table != null;
  const rows = table || largest === undefined ? largest : Math.min(largest, pageSize);
  const picked = useWorkspace((s) => s.activeResult[id]);
  const display = useWorkspace((s) => s.resultDisplay[id]);
  const shown = console ? shownResult(console.run, picked) : undefined;
  const chart = shown !== undefined && display?.key === resultKey(shown) && display.display === "chart";
  const heights = cellHeights({
    viewport,
    lines: text.split("\n").length,
    lineHeight: Math.round(codeSize * 1.54),
    results: hasResults
      ? {
          table,
          rows,
          rowHeight: gridRowHeight(codeSize, density),
          chrome: Math.round(80 * DENSITY_FACTOR[density]),
          pager: !table && largest !== undefined && hasResultPager(largest),
          chart,
        }
      : null,
    focused,
    dragged,
  });
  useEffect(() => {
    if (focused && !dragging) cell.current?.scrollIntoView({ block: "nearest" });
  }, [focused, dragging, dragged, heights.results, viewport]);
  if (!tab) return null;
  const activate = (focus = false) => {
    setFocusOnActivate(focus);
    useWorkspace.getState().setActive(id);
    activateEditor(id);
  };
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
        style={{ scrollMarginBlock: CELL_SCROLL_MARGIN }}
        className={cx("notebook-cell", active && "notebook-cell-active")}
        onFocusCapture={() => {
          if (!active) activate();
        }}
      >
        <div className="notebook-cell-heading">
          <span className="font-mono text-ui-11 text-dim tabular-nums">{String(index + 1).padStart(2, "0")}</span>
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
                const direction = ["ArrowLeft", "ArrowUp"].includes(e.key) ? -1 : ["ArrowRight", "ArrowDown"].includes(e.key) ? 1 : 0;
                if (!direction && e.key !== "Home" && e.key !== "End") return;
                e.preventDefault();
                const titles = [...(cell.current?.parentElement?.querySelectorAll<HTMLButtonElement>("button[data-tab-id]") ?? [])];
                const at = titles.indexOf(e.currentTarget);
                const next = e.key === "Home" ? titles[0] : e.key === "End" ? titles.at(-1) : titles[(at + direction + titles.length) % titles.length];
                next?.focus();
              }}
              onDoubleClick={() => {
                if (!tab.file) {
                  setDraftTitle(tab.title);
                  setRenaming(true);
                }
              }}
              className="min-w-0 flex-1 truncate text-left text-ui-12 font-medium text-fg"
              title={tab.file?.binding.path ?? tab.title}
            >
              {tab.title}
              {unsaved ? " ·" : ""}
            </button>
          )}
          {pending && (
            <span
              className={cx("size-1.5 shrink-0 rounded-full", console?.txn === "Aborted" ? "bg-danger" : "bg-amber")}
              role="img"
              aria-label={console?.txn === "Aborted" ? "Transaction aborted" : "Uncommitted transaction"}
            />
          )}
          <span className="text-ui-10 text-dim" title="Run this cell">
            ⌘ ↵
          </span>
          <Button
            data-testid={active ? (cancellable ? "cancel-btn" : "run-btn") : undefined}
            variant={cancellable ? "outline" : "primary"}
            className="notebook-run"
            disabled={busy && !cancellable}
            onClick={() => {
              activate();
              if (cancellable) cancelActive();
              else runActive({ kind: "all" });
            }}
          >
            {cancellable ? <SquareIcon size={11} /> : <PlayIcon size={12} />}
            {cancellable ? "Stop" : "Run"}
          </Button>
          <Menu.Root modal={false}>
            <Menu.Trigger asChild>
              <IconButton aria-label={`Cell actions for ${tab.title}`} title="Cell actions">
                <EllipsisIcon size={15} />
              </IconButton>
            </Menu.Trigger>
            <MenuContent align="end">
              <MenuItem
                onSelect={() => {
                  activate();
                  void formatActive();
                }}
              >
                <ListIndentIncreaseIcon size={14} />
                Format SQL
              </MenuItem>
              <MenuItem onSelect={() => openSqlTab({ db: tab.console.db, schema: tab.console.schema, text, title: `${tab.title} copy` })}>
                Duplicate cell
              </MenuItem>
              {explainSupported(databaseKind) && (
                <>
                  <MenuItem
                    onSelect={() => {
                      activate();
                      explainActive("Explain");
                    }}
                  >
                    Explain
                  </MenuItem>
                  <MenuItem
                    onSelect={() => {
                      activate();
                      explainActive("ExplainAnalyze");
                    }}
                  >
                    Explain Analyze
                  </MenuItem>
                </>
              )}
              <MenuItem onSelect={() => void closeTab(id)}>
                <XIcon size={14} />
                Close cell
              </MenuItem>
            </MenuContent>
          </Menu.Root>
        </div>
        <div
          className="notebook-editor"
          style={{ height: heights.editor }}
          onMouseDown={() => {
            if (!active) activate(true);
          }}
        >
          {active ? (
            <SqlEditor tabId={id} autoFocus={focusOnActivate} />
          ) : (
            <SqlText mode="view" text={text} databaseKind={databaseKind} label={`SQL in ${tab.title}`} testId={`cell-preview-${id}`} />
          )}
        </div>
        {heights.results !== null && (
          <>
            <CellResizeHandle {...editorHandle} lineColor="bg-border" onDragChange={setDragging} />
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
      <CellResizeHandle {...lowerHandle} lineColor="bg-transparent" onDragChange={setDragging} />
      <AddCell after={id} />
    </>
  );
}

function useFocusAfterClick() {
  const active = useWorkspace((s) => s.active);
  const [focused, setFocused] = useState(active);
  const pressed = useRef(false);
  useLayoutEffect(() => {
    if (!pressed.current) setFocused(active);
  }, [active]);
  useEffect(() => {
    const release = () => {
      if (!pressed.current) return;
      pressed.current = false;
      setTimeout(() => setFocused(useWorkspace.getState().active), 0);
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

export function NotebookWorkspace() {
  const order = useWorkspace((s) => s.order);
  const { focused, onPointerDownCapture } = useFocusAfterClick();
  const scroller = useRef<HTMLDivElement>(null);
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
  return (
    <div ref={scroller} data-testid="notebook" className="notebook-workspace" onPointerDownCapture={onPointerDownCapture}>
      {order.length === 0 ? (
        <StartCommands />
      ) : (
        order.map((id, index) => <NotebookCell key={id} id={id} index={index} viewport={viewport} focused={focused === id} />)
      )}
    </div>
  );
}
