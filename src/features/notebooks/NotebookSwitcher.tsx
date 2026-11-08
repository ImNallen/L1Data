import * as Dialog from "@radix-ui/react-dialog";
import * as Menu from "@radix-ui/react-dropdown-menu";
import { BookOpenIcon, CheckIcon, EllipsisIcon, PlusIcon, SearchIcon } from "lucide-react";
import { Fragment, useEffect, useId, useRef, useState, type KeyboardEvent, type SyntheticEvent } from "react";
import type { NotebookId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { activeId, useWorkspace } from "../../state/workspace";
import { isMac } from "../../shell/hostedWindow";
import { ACTIVE_OPTION, cx, DialogSurface, Kbd, MenuContent, MenuItem } from "../../ui/primitives";
import { plural } from "../editor/runActions";
import { focusDestination } from "../navigation/navigationActions";
import { deleteNotebook, startNotebook } from "./notebookActions";
import { switcherGroups, type SwitcherRow } from "./switcherItems";

const MAC = typeof navigator !== "undefined" && isMac(navigator.userAgent);
const DELETE_KEY = MAC ? "⌘⌫" : "Ctrl+Backspace";

const ws = () => useWorkspace.getState();

/** Set while a choice closes the switcher, so focus goes to the cell it shows instead of back to the opener. */
let activating = false;

function activate(row: SwitcherRow): void {
  activating = true;
  ws().close();
  ws().closeSettings();
  if (row.kind === "new") {
    focusDestination(startNotebook());
    return;
  }
  ws().showNotebook(row.notebook.id);
  const cell = activeId(ws());
  if (cell) focusDestination(cell);
}

type Renaming = { id: NotebookId; draft: string };

/** React bubbles events from the row's portalled menu through the row, so the row acts only on its own DOM. */
const inRow = (event: SyntheticEvent) => event.currentTarget.contains(event.target as Node);

function RenameInput({ renaming, title, onDraft, onSave }: { renaming: Renaming; title: string; onDraft: (draft: string) => void; onSave: () => void }) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  return (
    <input
      ref={ref}
      data-testid="notebook-rename-input"
      aria-label={`Rename ${title}`}
      value={renaming.draft}
      onChange={(event) => onDraft(event.target.value)}
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => {
        event.stopPropagation();
        if (event.key === "Enter") onSave();
      }}
      className="h-6 min-w-0 flex-1 rounded border border-focus bg-bg px-1.5 text-ui-13.5 text-fg outline-none"
    />
  );
}

function SwitcherBrowser({ renaming, setRenaming }: { renaming: Renaming | null; setRenaming: (r: Renaming | null) => void }) {
  const notebooks = useWorkspace((s) => s.notebooks);
  const shown = useWorkspace((s) => s.shown);
  const configs = useWorkspace((s) => s.configs);
  const consoles = useLive((s) => s.consoles);
  const renameNotebook = useWorkspace((s) => s.renameNotebook);
  const [query, setQuery] = useState("");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const composing = useRef(false);
  const listId = useId();
  const { groups, total } = switcherGroups({ notebooks, shown, configs, consoles }, query);
  const rows = groups.flatMap((g) => g.rows);
  const matching = rows.filter((r) => r.kind === "notebook").length;
  const found = rows.findIndex((r) => r.key === selectedKey);
  const selectedIndex =
    found >= 0
      ? found
      : Math.max(
          0,
          rows.findIndex((r) => !(r.kind === "notebook" && r.open)),
        );
  const selected = rows[selectedIndex];
  const optionId = (index: number) => `${listId}-${index}`;
  useEffect(() => {
    document.getElementById(optionId(selectedIndex))?.scrollIntoView({ block: "nearest" });
  });

  const startRename = (row: SwitcherRow | undefined) => {
    if (row?.kind === "notebook") setRenaming({ id: row.notebook.id, draft: row.notebook.title });
  };
  const finishRename = () => {
    if (renaming) renameNotebook(renaming.id, renaming.draft);
    setRenaming(null);
    input.current?.focus();
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (composing.current || event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const next = rows[(selectedIndex + (event.key === "ArrowDown" ? 1 : -1) + rows.length) % rows.length];
      if (next) setSelectedKey(next.key);
    } else if (event.key === "Enter" && selected) {
      event.preventDefault();
      activate(selected);
    } else if (event.key === "F2" && selected?.kind === "notebook") {
      event.preventDefault();
      startRename(selected);
    } else if (event.key === "Backspace" && (MAC ? event.metaKey : event.ctrlKey) && selected?.kind === "notebook") {
      event.preventDefault();
      void deleteNotebook(selected.notebook.id);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col" onCompositionStart={() => (composing.current = true)} onCompositionEnd={() => (composing.current = false)}>
      <div className="flex shrink-0 items-center gap-3 border-b border-control px-4">
        <SearchIcon size={18} className="text-muted" />
        <input
          ref={input}
          data-testid="notebook-switcher-input"
          role="combobox"
          aria-label="Search notebooks"
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls={listId}
          aria-activedescendant={selected ? optionId(selectedIndex) : undefined}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Search notebooks…"
          autoComplete="off"
          spellCheck={false}
          className="h-14 min-w-0 flex-1 bg-transparent text-ui-14 text-fg outline-none placeholder:text-muted"
        />
        <Dialog.Close aria-label="Close notebooks" className="rounded text-dim hover:text-fg">
          <Kbd>esc</Kbd>
        </Dialog.Close>
      </div>
      <div id={listId} role="listbox" aria-label="Notebooks" className="max-h-96 min-h-0 flex-1 overflow-auto px-2 py-2">
        {groups.map((group) => (
          <Fragment key={group.label}>
            <div role="presentation" className="px-3 pt-1.5 pb-1 text-ui-11.5 text-dim">
              {group.label}
            </div>
            {group.rows.map((row) => {
              const index = rows.indexOf(row);
              const isSelected = index === selectedIndex;
              const editing = row.kind === "notebook" && renaming?.id === row.notebook.id ? renaming : null;
              const title = row.kind === "notebook" ? row.notebook.title : "New notebook";
              return (
                <div
                  key={row.key}
                  id={optionId(index)}
                  role="option"
                  aria-selected={isSelected}
                  aria-label={title}
                  data-testid={`notebook-switcher-${row.kind === "new" ? "new" : row.notebook.id}`}
                  onMouseMove={(event) => inRow(event) && setSelectedKey(row.key)}
                  onMouseDown={(event) => inRow(event) && !editing && event.preventDefault()}
                  onClick={(event) => inRow(event) && !editing && activate(row)}
                  className={cx("group flex min-h-12 cursor-default items-center gap-3 rounded-lg px-3 py-1.5", isSelected ? ACTIVE_OPTION : "hover:bg-hover")}
                >
                  {row.kind === "new" ? <PlusIcon size={17} className="shrink-0 text-muted" /> : <BookOpenIcon size={17} className="shrink-0 text-muted" />}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 text-ui-13.5 text-fg">
                      {editing ? (
                        <RenameInput renaming={editing} title={title} onDraft={(draft) => setRenaming({ id: editing.id, draft })} onSave={finishRename} />
                      ) : (
                        <span className="truncate">{title}</span>
                      )}
                      {row.kind === "notebook" && !editing && (
                        <span className="shrink-0 text-ui-11.5 text-dim">{plural(row.notebook.cells.length, "cell")}</span>
                      )}
                    </div>
                    {row.context && (
                      <div className="truncate text-ui-11.5 text-muted" title={row.context}>
                        {row.context}
                      </div>
                    )}
                  </div>
                  {row.kind === "notebook" && row.open && <CheckIcon size={15} className="shrink-0 text-run" />}
                  {row.kind === "notebook" && <span className="shrink-0 text-ui-11.5 text-dim">{row.action}</span>}
                  {row.kind === "notebook" && !editing && (
                    <Menu.Root modal={false}>
                      <Menu.Trigger
                        aria-label={`${title} actions`}
                        data-testid={`notebook-actions-${row.notebook.id}`}
                        onClick={(event) => event.stopPropagation()}
                        className={cx(
                          "inline-grid size-6 shrink-0 place-items-center rounded-md text-muted hover:bg-hover hover:text-fg data-[state=open]:opacity-100",
                          isSelected ? "opacity-100" : "opacity-0 group-hover:opacity-100 focus-visible:opacity-100",
                        )}
                      >
                        <EllipsisIcon size={14} />
                      </Menu.Trigger>
                      <MenuContent
                        align="end"
                        onCloseAutoFocus={(event) => {
                          event.preventDefault();
                          if (useWorkspace.getState().overlay.kind !== "notebooks") return;
                          (document.querySelector<HTMLInputElement>('[data-testid="notebook-rename-input"]') ?? input.current)?.focus();
                        }}
                      >
                        <MenuItem testId="notebook-rename" onSelect={() => startRename(row)}>
                          <span className="flex-1">Rename</span>
                          <Kbd tone="menu">F2</Kbd>
                        </MenuItem>
                        <MenuItem testId="notebook-delete" danger onSelect={() => void deleteNotebook(row.notebook.id)}>
                          <span className="flex-1">Delete</span>
                          <Kbd tone="menu">{DELETE_KEY}</Kbd>
                        </MenuItem>
                      </MenuContent>
                    </Menu.Root>
                  )}
                </div>
              );
            })}
          </Fragment>
        ))}
      </div>
      <div className="flex min-h-10 shrink-0 items-center gap-3 border-t border-control px-4 text-ui-11.5 text-dim">
        <span>
          <Kbd>↑↓</Kbd> Navigate
        </span>
        <span>
          <Kbd>↵</Kbd> Open
        </span>
        <span>
          <Kbd>esc</Kbd> Close
        </span>
        <span data-testid="notebook-switcher-count" role="status" className="ml-auto">
          {matching < total ? `${matching} of ${plural(total, "notebook")}` : plural(total, "notebook")}
        </span>
      </div>
    </div>
  );
}

export function NotebookSwitcher() {
  const open = useWorkspace((s) => s.overlay.kind === "notebooks");
  const close = useWorkspace((s) => s.close);
  const [renaming, setRenaming] = useState<Renaming | null>(null);
  const opener = useRef<Element | null>(null);
  return (
    <Dialog.Root open={open} onOpenChange={(value) => !value && close()}>
      <Dialog.Portal>
        <DialogSurface
          data-testid="notebook-switcher"
          aria-describedby={undefined}
          className="flex w-[640px] flex-col overflow-hidden"
          onOpenAutoFocus={() => {
            opener.current = document.activeElement;
            activating = false;
            setRenaming(null);
          }}
          onEscapeKeyDown={(event) => {
            if (event.isComposing || event.keyCode === 229) event.preventDefault();
            else if (renaming) {
              event.preventDefault();
              setRenaming(null);
              document.querySelector<HTMLInputElement>('[data-testid="notebook-switcher-input"]')?.focus();
            }
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            if (!activating && useWorkspace.getState().overlay.kind === "none" && opener.current instanceof HTMLElement && opener.current.isConnected)
              opener.current.focus();
          }}
        >
          <Dialog.Title className="sr-only">Switch notebook</Dialog.Title>
          <SwitcherBrowser renaming={renaming} setRenaming={setRenaming} />
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
