import * as Dialog from "@radix-ui/react-dialog";
import { DatabaseIcon, FileTextIcon, SearchIcon, Table2Icon } from "lucide-react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { useCatalog } from "../../state/catalog";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { ACTIVE_OPTION, cx, DialogSurface, Kbd } from "../../ui/primitives";
import { activateDestination, focusDestination } from "./navigationActions";
import { navigationItems, searchNavigation, type NavigationItem } from "./navigationItems";

function NavigationBrowser({ activate }: { activate: (item: NavigationItem) => void }) {
  const tabs = useWorkspace((s) => s.tabs);
  const notebooks = useWorkspace((s) => s.notebooks);
  const shown = useWorkspace((s) => s.shown);
  const configs = useWorkspace((s) => s.configs);
  const connections = useLive((s) => s.connections);
  const views = useCatalog((s) => s.views);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const composing = useRef(false);
  const listId = useId();
  const { items, total } = searchNavigation(navigationItems({ tabs, notebooks, shown, configs, connections, views }), query);
  const selectedIndex = Math.max(
    0,
    items.findIndex((item) => item.id === selectedId),
  );
  const selected = items[selectedIndex];
  const optionId = `${listId}-${selectedIndex}`;
  useEffect(() => {
    document.getElementById(optionId)?.scrollIntoView({ block: "nearest" });
  }, [optionId, selected?.id]);

  return (
    <div className="flex min-h-0 flex-1 flex-col" onCompositionStart={() => (composing.current = true)} onCompositionEnd={() => (composing.current = false)}>
      <div className="flex shrink-0 items-center gap-3 border-b border-control px-4">
        <SearchIcon size={18} className="text-muted" />
        <input
          data-testid="navigation-input"
          role="combobox"
          aria-label="Quick navigation"
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls={listId}
          aria-activedescendant={selected ? optionId : undefined}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (composing.current || event.nativeEvent.isComposing || event.keyCode === 229) return;
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              if (!selected) return;
              const next = items[(selectedIndex + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length];
              if (next) setSelectedId(next.id);
            } else if (event.key === "Enter" && selected) {
              event.preventDefault();
              activate(selected);
            }
          }}
          placeholder="Go to a database, tab, table or view…"
          autoComplete="off"
          spellCheck={false}
          className="h-14 min-w-0 flex-1 bg-transparent text-ui-14 text-fg outline-none placeholder:text-muted"
        />
        <Dialog.Close aria-label="Close quick navigation" data-testid="navigation-close" className="rounded text-dim hover:text-fg">
          <Kbd>esc</Kbd>
        </Dialog.Close>
      </div>
      <p id={`${listId}-scope`} data-testid="navigation-scope" className="shrink-0 px-4 py-2 text-ui-11.5 text-dim">
        Open tabs and discovered databases, including hidden databases. Tables and views from loaded catalogs only.
      </p>
      <div
        id={listId}
        role="listbox"
        aria-label="Destinations"
        aria-describedby={`${listId}-scope`}
        className="max-h-80 min-h-0 flex-1 overflow-auto px-2 pb-2"
      >
        {items.map((item, index) => {
          const Icon = item.destination.kind === "database" ? DatabaseIcon : item.destination.kind === "tab" ? FileTextIcon : Table2Icon;
          return (
            <div
              key={item.id}
              id={`${listId}-${index}`}
              role="option"
              aria-selected={index === selectedIndex}
              data-testid={`navigation-item-${item.id}`}
              onMouseMove={() => setSelectedId(item.id)}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => activate(item)}
              className={cx("flex cursor-default items-center gap-3 rounded-lg px-3 py-2", index === selectedIndex ? ACTIVE_OPTION : "hover:bg-hover")}
            >
              <Icon size={17} className="shrink-0 text-muted" />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-ui-13.5 text-fg">
                  <span className="truncate">{item.label}</span>
                  <span className="shrink-0 text-ui-11.5 text-dim">{item.kindLabel}</span>
                </div>
                <div className="truncate text-ui-11.5 text-muted" title={item.context}>
                  {item.context}
                </div>
              </div>
              <span className="max-w-[180px] shrink-0 truncate text-ui-11.5 text-dim" title={item.action}>
                {item.action}
              </span>
            </div>
          );
        })}
        {items.length === 0 && (
          <p data-testid="navigation-empty" role="status" className="px-3 py-8 text-center text-ui-13 text-dim">
            No destinations found.
          </p>
        )}
      </div>
      <div className="flex min-h-10 shrink-0 items-center gap-3 border-t border-control px-4 text-ui-11.5 text-dim">
        <span>
          <Kbd>↑↓</Kbd> Navigate
        </span>
        <span>
          <Kbd>↵</Kbd> Open
        </span>
        <span data-testid="navigation-count" role="status" className="ml-auto">
          {total > items.length ? `${items.length} of ${total}` : total} destinations
        </span>
      </div>
    </div>
  );
}

export function NavigationPalette() {
  const open = useWorkspace((s) => s.overlay.kind === "navigation");
  const close = useWorkspace((s) => s.close);
  const opener = useRef<Element | null>(null);
  const activating = useRef(false);
  useLayoutEffect(() => {
    if (open) activating.current = false;
  }, [open]);
  const restoreFocus = () => {
    if (useWorkspace.getState().overlay.kind === "none" && opener.current instanceof HTMLElement && opener.current.isConnected) opener.current.focus();
  };
  return (
    <Dialog.Root open={open} onOpenChange={(value) => !value && close()}>
      <Dialog.Portal>
        <DialogSurface
          data-testid="navigation-palette"
          aria-describedby={undefined}
          className="flex w-[640px] flex-col overflow-hidden"
          onOpenAutoFocus={() => {
            opener.current = document.activeElement;
          }}
          onEscapeKeyDown={(event) => {
            if (event.isComposing || event.keyCode === 229) event.preventDefault();
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            if (!activating.current) restoreFocus();
          }}
        >
          <Dialog.Title className="sr-only">Quick navigation</Dialog.Title>
          <NavigationBrowser
            activate={(item) => {
              if (activating.current) return;
              activating.current = true;
              void activateDestination(item.destination).then((tabId) => {
                if (tabId) focusDestination(tabId);
                else restoreFocus();
              });
            }}
          />
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
