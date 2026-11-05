import * as Dialog from "@radix-ui/react-dialog";
import { SearchIcon } from "lucide-react";
import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState, useSyncExternalStore, type RefObject } from "react";
import { docOf, focusEditor, onBuffersChanged } from "../features/editor/editorStates";
import { useWorkspace, type Screen } from "../state/workspace";
import type { TabId } from "../ipc/bindings";
import { ACTIVE_OPTION, cx, DialogSurface, Kbd } from "../ui/primitives";
import { COMMAND_GROUPS, COMMANDS, commandAvailable, commandLabel, shortcutLabel, type Command } from "./commands";

function CommandBrowser({ activate, composing }: { activate: (command: Command) => void; composing: RefObject<boolean> }) {
  useWorkspace((s) => s.tabs);
  useWorkspace((s) => s.screen);
  const active = useWorkspace((s) => s.active);
  useSyncExternalStore(onBuffersChanged, () => (active ? docOf(active) : undefined));
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const listId = useId();
  const words = query.toLowerCase().trim().split(/\s+/);
  const matching = COMMANDS.filter((command) => command.palette !== false && commandAvailable(command)).filter((command) =>
    words.every((word) => commandLabel(command).toLowerCase().includes(word)),
  );
  const items = COMMAND_GROUPS.flatMap((group) => matching.filter((command) => command.group === group));
  const selectedIndex = Math.max(
    0,
    items.findIndex((command) => command.id === selectedId),
  );
  const selected = items[selectedIndex];
  const optionId = `${listId}-${selectedIndex}`;
  useEffect(() => {
    document.getElementById(optionId)?.scrollIntoView({ block: "nearest" });
  }, [optionId, selected?.id]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-3 border-b border-control px-4">
        <SearchIcon size={18} className="text-muted" />
        <input
          data-testid="command-input"
          role="combobox"
          aria-label="Command panel"
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
          placeholder="Search commands…"
          autoComplete="off"
          spellCheck={false}
          className="h-14 min-w-0 flex-1 bg-transparent text-ui-14 text-fg outline-none placeholder:text-muted"
        />
        <Dialog.Close aria-label="Close command panel" data-testid="command-close" className="rounded text-dim hover:text-fg">
          <Kbd>esc</Kbd>
        </Dialog.Close>
      </div>
      <div id={listId} role="listbox" aria-label="Commands" className="max-h-80 min-h-0 flex-1 overflow-auto p-2">
        {items.map((command, index) => (
          <Fragment key={command.id}>
            {(index === 0 || items[index - 1]?.group !== command.group) && (
              <div role="presentation" className="px-3 pt-3 pb-1 text-ui-11.5 font-medium text-dim">
                {command.group}
              </div>
            )}
            <div
              id={`${listId}-${index}`}
              role="option"
              aria-selected={index === selectedIndex}
              data-testid={`command-item-${command.id}`}
              onMouseMove={() => setSelectedId(command.id)}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => activate(command)}
              className={cx(
                "flex cursor-default items-center justify-between gap-3 rounded-lg px-3 py-2 text-ui-13.5 text-fg",
                index === selectedIndex ? ACTIVE_OPTION : "hover:bg-hover",
              )}
            >
              <span>{commandLabel(command)}</span>
              {command.key && <Kbd className="shrink-0 text-muted">{shortcutLabel(command)}</Kbd>}
            </div>
          </Fragment>
        ))}
        {items.length === 0 && (
          <p data-testid="command-empty" role="status" className="px-3 py-8 text-center text-ui-13 text-dim">
            No commands found.
          </p>
        )}
      </div>
      <div className="flex min-h-10 shrink-0 items-center gap-3 border-t border-control px-4 text-ui-11.5 text-dim">
        <span>
          <Kbd>↑↓</Kbd> Navigate
        </span>
        <span>
          <Kbd>↵</Kbd> Run command
        </span>
      </div>
    </div>
  );
}

export function CommandPalette() {
  const open = useWorkspace((s) => s.overlay.kind === "commands");
  const opener = useRef<{ element: Element | null; active: TabId | null; screen: Screen["kind"] } | null>(null);
  const activating = useRef(false);
  const composing = useRef(false);
  useLayoutEffect(() => {
    if (open) {
      activating.current = false;
      composing.current = false;
    }
  }, [open]);
  const close = () => {
    const workspace = useWorkspace.getState();
    if (workspace.overlay.kind === "commands") workspace.close();
  };
  const restoreFocus = () => {
    if (useWorkspace.getState().overlay.kind !== "none") return;
    const element = opener.current?.element;
    if (element instanceof HTMLElement && element.isConnected) element.focus();
  };

  return (
    <Dialog.Root open={open} onOpenChange={(value) => !value && close()}>
      <Dialog.Portal>
        <DialogSurface
          data-testid="command-palette"
          aria-describedby={undefined}
          className="flex w-[560px] flex-col overflow-hidden"
          onCompositionStart={() => (composing.current = true)}
          onCompositionEnd={() => (composing.current = false)}
          onOpenAutoFocus={() => {
            const { active, screen } = useWorkspace.getState();
            opener.current = { element: document.activeElement, active, screen: screen.kind };
          }}
          onEscapeKeyDown={(event) => {
            if (composing.current || event.isComposing || event.keyCode === 229) event.preventDefault();
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            if (!activating.current) return restoreFocus();
            requestAnimationFrame(() => {
              const { overlay, screen, active } = useWorkspace.getState();
              if (overlay.kind !== "none" || document.querySelector('[role="dialog"], [role="alertdialog"]')) return;
              if (screen.kind === opener.current?.screen && active === opener.current.active) {
                if (document.activeElement === document.body) restoreFocus();
              } else if (screen.kind === "workspace") {
                if (active) focusEditor();
                else document.querySelector<HTMLElement>('[data-testid="start-new-query"]')?.focus();
              }
            });
          }}
        >
          <Dialog.Title className="sr-only">Command panel</Dialog.Title>
          <CommandBrowser
            composing={composing}
            activate={(command) => {
              if (activating.current || useWorkspace.getState().overlay.kind !== "commands" || !commandAvailable(command)) return;
              activating.current = true;
              close();
              command.run();
            }}
          />
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
