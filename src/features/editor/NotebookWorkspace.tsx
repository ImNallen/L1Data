import { DENSITY_FACTOR, gridRowHeight } from "../../shell/appearance";
import { StartCommands } from "../../shell/StartCommands";
import * as Menu from "@radix-ui/react-dropdown-menu";
import { EllipsisIcon, ListIndentIncreaseIcon, PlayIcon, PlusIcon, SquareIcon, XIcon } from "lucide-react";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { canCancel, hasUncommittedWork, isBusy, rowResults } from "../../state/status";
import { useWorkspace } from "../../state/workspace";
import { Button, cx, IconButton, MenuContent, MenuItem } from "../../ui/primitives";
import { ResultsPane } from "../results/ResultsPane";
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

function NotebookCell({ id, index }: { id: TabId; index: number }) {
  const tab = useWorkspace((s) => s.tabs[id]);
  const active = useWorkspace((s) => s.active === id);
  const showResults = useWorkspace((s) => s.layout.results);
  const codeSize = useWorkspace((s) => s.settings.codeFont.size);
  const density = useWorkspace((s) => s.settings.density);
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
  useEffect(() => {
    if (active) cell.current?.scrollIntoView({ block: "nearest" });
  }, [active]);
  if (!tab) return null;
  const activate = (focus = false) => {
    setFocusOnActivate(focus);
    useWorkspace.getState().setActive(id);
    activateEditor(id);
  };
  const busy = isBusy(console);
  const pending = hasUncommittedWork(console);
  const cancellable = canCancel(console);
  const hasResults = showResults && ((tab.table !== null && active) || (console && console.run.kind !== "Idle"));
  const editorHeight = Math.max(64, Math.min(300, text.split("\n").length * Math.round(codeSize * 1.54) + 28));
  const rows = console ? rowResults(console.run)[0]?.meta.rowCount : undefined;
  const resultsHeight = tab.table ? 380 : Math.max(128, Math.round(80 * DENSITY_FACTOR[density]) + Math.min(rows ?? 4, 10) * gridRowHeight(codeSize, density));
  return (
    <>
      <section
        ref={cell}
        data-testid={`notebook-cell-${id}`}
        data-active={active}
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
          style={{ height: editorHeight }}
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
        {hasResults && (
          <div
            className="notebook-results"
            style={{ height: resultsHeight }}
            onMouseDown={() => {
              if (!active) activate();
            }}
          >
            <ResultsPane tabId={id} />
          </div>
        )}
      </section>
      <AddCell after={id} />
    </>
  );
}

export function NotebookWorkspace() {
  const order = useWorkspace((s) => s.order);
  return (
    <div data-testid="notebook" className="notebook-workspace">
      {order.length === 0 ? <StartCommands /> : order.map((id, index) => <NotebookCell key={id} id={id} index={index} />)}
    </div>
  );
}
