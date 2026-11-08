import { FileXIcon, TableIcon, XIcon } from "lucide-react";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { closeTab } from "../features/editor/closeTab";
import { isDirty, onBuffersChanged } from "../features/editor/editorStates";
import type { TabId } from "../ipc/bindings";
import { useLive } from "../state/live";
import { hasUncommittedWork } from "../state/status";
import { activeId, shownNotebook, useWorkspace, type Tab } from "../state/workspace";
import { cx } from "../ui/primitives";

function fileTooltip(tab: Tab): string | undefined {
  if (!tab.file) return undefined;
  const { path } = tab.file.binding;
  switch (tab.file.health) {
    case "ok":
      return path;
    case "missing":
      return `${path} was moved or deleted. Saving writes it again.`;
    case "changed":
      return `${path} changed on disk. Saving asks before overwriting it.`;
    case "unreadable":
      return `L1 Data couldn't read ${path} again. Saving asks before overwriting it.`;
    default: {
      const unreachable: never = tab.file.health;
      return unreachable;
    }
  }
}

function TabItem({ id, onClose }: { id: TabId; onClose: (id: TabId, trigger: HTMLButtonElement, keyboard: boolean) => void }) {
  const tab = useWorkspace((s) => s.tabs[id]);
  const active = useWorkspace((s) => activeId(s) === id);
  const setActive = useWorkspace((s) => s.setActive);
  const renameTab = useWorkspace((s) => s.renameTab);
  const dirty = useLive((s) => hasUncommittedWork(s.consoles[id]));
  const aborted = useLive((s) => s.consoles[id]?.txn === "Aborted");
  const unsaved = useSyncExternalStore(onBuffersChanged, () => isDirty(id));
  const [editing, setEditing] = useState(false);
  const activation = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (active) activation.current?.parentElement?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [active]);
  if (!tab) return null;
  const missing = tab.file?.health === "missing";

  return (
    <div
      onMouseDown={(e) => {
        if (e.button === 1) {
          e.preventDefault();
          void closeTab(id);
        }
      }}
      title={fileTooltip(tab)}
      className={cx(
        "group relative flex h-7 max-w-[260px] min-w-0 shrink-0 items-center rounded-md border pr-1",
        active ? "border-control bg-pill text-fg" : "border-transparent text-muted hover:bg-hover hover:text-fg",
      )}
    >
      <button
        ref={activation}
        type="button"
        data-testid={`tab-${id}`}
        data-tab-id={id}
        data-active={active}
        role="tab"
        aria-selected={active}
        tabIndex={active ? 0 : -1}
        onClick={() => setActive(id)}
        onDoubleClick={() => !tab.file && setEditing(true)}
        className={cx("flex h-full min-w-0 items-center gap-2 rounded px-2", editing && "invisible")}
      >
        {tab.table ? (
          <TableIcon size={14} className="shrink-0 text-muted" />
        ) : (
          <span aria-hidden="true" className="font-mono text-ui-10.5 font-bold tracking-tight text-badge">
            SQL
          </span>
        )}
        <span className={cx("truncate text-ui-13.5", missing && "line-through")}>{tab.title}</span>
        {missing && <FileXIcon data-testid={`tab-file-missing-${id}`} aria-label="File missing" size={13} className="-ml-1 shrink-0 text-amber" />}
        {unsaved && (
          <span
            data-testid={`tab-unsaved-${id}`}
            role="img"
            aria-label="Unsaved changes"
            className="-ml-1 size-2 shrink-0 rounded-full border-[1.5px] border-focus"
          />
        )}
        {dirty && (
          <span
            data-testid={`tab-dirty-${id}`}
            role="img"
            aria-label={aborted ? "Transaction aborted" : "Transaction open"}
            className={cx("size-1.5 shrink-0 rounded-full", aborted ? "bg-danger" : "bg-amber")}
          />
        )}
      </button>
      {editing && (
        <input
          autoFocus
          aria-label="Tab name"
          defaultValue={tab.title}
          onFocus={(e) => e.currentTarget.select()}
          onBlur={(e) => {
            const title = e.currentTarget.value.trim();
            if (title) renameTab(id, title);
            setEditing(false);
          }}
          onKeyDown={(e) => {
            if (e.key !== "Enter" && e.key !== "Escape") return;
            e.preventDefault();
            if (e.key === "Enter") e.currentTarget.blur();
            else setEditing(false);
            requestAnimationFrame(() => activation.current?.focus());
          }}
          className="absolute inset-y-0 left-2 w-[calc(100%-2.5rem)] rounded bg-pill text-ui-13.5 text-fg outline-none focus:ring-1 focus:ring-focus"
        />
      )}
      <button
        type="button"
        aria-label={`Close ${tab.title}`}
        data-testid={`tab-close-${id}`}
        tabIndex={active ? 0 : -1}
        onClick={(e) => onClose(id, e.currentTarget, e.detail === 0)}
        className={cx(
          "grid size-5 shrink-0 place-items-center rounded text-dim hover:bg-hover hover:text-fg",
          !active && "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100",
        )}
      >
        <XIcon size={13} />
      </button>
    </div>
  );
}

const NO_CELLS: TabId[] = [];

export function TabBar() {
  const order = useWorkspace((s) => shownNotebook(s)?.cells ?? NO_CELLS);
  const strip = useRef<HTMLDivElement>(null);
  const focusTab = (id: TabId | null) => {
    const target = Array.from(strip.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]') ?? []).find((el) => el.dataset.tabId === id);
    target?.focus();
    return target;
  };
  const close = async (id: TabId, trigger: HTMLButtonElement, keyboard: boolean) => {
    await closeTab(id);
    if (!keyboard) return;
    requestAnimationFrame(() => {
      const { tabs } = useWorkspace.getState();
      if (tabs[id]) trigger.focus();
      else if (!focusTab(activeId(useWorkspace.getState()))) document.querySelector<HTMLButtonElement>('[data-testid="start-new-query"]')?.focus();
    });
  };
  return (
    <div
      ref={strip}
      role="tablist"
      aria-label="Query tabs"
      onKeyDown={(e) => {
        if (!(e.target instanceof HTMLButtonElement) || e.target.getAttribute("role") !== "tab") return;
        const id = e.target.dataset.tabId;
        if (!id) return;
        const index = order.indexOf(id);
        let next: TabId | undefined;
        switch (e.key) {
          case "ArrowLeft":
            next = order[(index - 1 + order.length) % order.length];
            break;
          case "ArrowRight":
            next = order[(index + 1) % order.length];
            break;
          case "Home":
            next = order[0];
            break;
          case "End":
            next = order.at(-1);
            break;
          default:
            return;
        }
        e.preventDefault();
        if (next) {
          useWorkspace.getState().setActive(next);
          requestAnimationFrame(() => focusTab(next));
        }
      }}
      className="flex h-10 shrink-0 items-center gap-1 overflow-x-auto px-2 [scrollbar-width:none]"
    >
      {order.map((id) => (
        <TabItem key={id} id={id} onClose={(...args) => void close(...args)} />
      ))}
    </div>
  );
}
