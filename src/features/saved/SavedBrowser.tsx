import { BookmarkIcon, BookmarkPlusIcon, CopyIcon, PencilIcon, Trash2Icon } from "lucide-react";
import { useEffect, useMemo, useReducer, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import type { SavedQuery, SavedQueryError, SavedQueryId } from "../../ipc/bindings";
import { savedQueryErrorText } from "../../ipc/errors";
import { commandForKey } from "../../shell/commands";
import { isMac } from "../../shell/hostedWindow";
import { databaseKindFor } from "../../state/databaseKind";
import { useLive } from "../../state/live";
import { activeTab, tabTarget, useWorkspace, type SavedEntry } from "../../state/workspace";
import { day } from "../../ui/day";
import { ACTIVE_OPTION, Button, cx, DatabaseBadge, Highlight, inputClass, Kbd } from "../../ui/primitives";
import { SqlText } from "../editor/SqlText";
import { canInsert, copySaved, deleteSaved, insertSaved, openSaved, saveActiveQuery, updateSaved } from "./savedActions";
import { keepSelection, rankSaved, savedKey, step, type SavedAction } from "./savedList";
import { dirty, listKeysLive, paneReducer, type EditPane, type Pane, type PaneEvent } from "./savedPane";

function when(at: number, now: Date): string {
  const label = day(at, now);
  return label === "Today" || label === "Yesterday" ? label.toLowerCase() : label;
}

function initialPane(entry: SavedEntry, items: SavedQuery[]): Pane {
  const query = entry.kind === "name" ? items.find((q) => q.id === entry.id) : undefined;
  return query ? paneReducer({ kind: "view" }, { type: "edit", query, focus: "name", closeOnDone: true }) : { kind: "view" };
}

const editErrorText = (e: SavedQueryError) => (e.kind === "NothingToSave" ? "Enter the SQL to save." : savedQueryErrorText(e));

/** While a query is being edited, Esc or a click outside steps the edit back through `holdOpen` instead of closing. */
export function SavedBrowser({
  entry,
  close,
  closeToEditor,
  holdOpen,
  controls,
}: {
  entry: SavedEntry;
  close: () => void;
  closeToEditor: () => void;
  holdOpen: RefObject<(() => boolean) | null>;
  controls: (locked: boolean) => ReactNode;
}) {
  const items = useLive((s) => s.savedQueries.items);
  const activeKind = useWorkspace((s) => {
    const tab = activeTab(s);
    return tab ? databaseKindFor(tabTarget(tab), s.configs) : null;
  });
  const [text, setText] = useState("");
  const ranked = useMemo(() => rankSaved(items, text), [items, text]);
  const [pick, setPick] = useState<{ id: SavedQueryId | null; index: number }>(() => {
    const id = entry.kind === "browse" ? null : entry.id;
    return {
      id,
      index: Math.max(
        0,
        items.findIndex((q) => q.id === id),
      ),
    };
  });
  const selectedId = keepSelection(ranked, pick.id, pick.index);
  const selected = ranked.find((r) => r.query.id === selectedId)?.query;
  const [pane, dispatch] = useReducer(paneReducer, undefined, () => initialPane(entry, items));
  const [copyError, setCopyError] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const insertable = canInsert();
  const mac = isMac(navigator.userAgent);
  const mod = mac ? "⌘" : "Ctrl+";
  const now = new Date();
  const frozen = pane.kind === "edit" || pane.kind === "confirmDiscard";

  useEffect(() => {
    listRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  }, [selectedId]);
  useEffect(() => {
    if (pane.kind === "view") searchRef.current?.focus();
    if (pane.kind === "done") closeToEditor();
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- runs on a pane change, and `closeToEditor` is new on every render
  }, [pane.kind]);

  function held() {
    if (listKeysLive(pane)) return false;
    dispatch({ type: "escape" });
    return true;
  }
  useEffect(() => {
    holdOpen.current = held;
    return () => {
      holdOpen.current = null;
    };
  });
  function select(id: SavedQueryId | null) {
    setPick({
      id,
      index: Math.max(
        0,
        ranked.findIndex((r) => r.query.id === id),
      ),
    });
    setCopyError(false);
    dispatch({ type: "selected", id });
  }
  function search(value: string) {
    setText(value);
    setPick({ id: null, index: 0 });
    setCopyError(false);
    dispatch({ type: "selected", id: null });
  }
  function act(action: SavedAction) {
    if (action === "next" || action === "previous") return select(step(ranked, selectedId, action));
    if (!selected) return;
    if (action === "copy") return void copySaved(selected).then((ok) => (ok ? closeToEditor() : setCopyError(true)));
    if (action === "primary" && insertable) insertSaved(selected);
    else openSaved(selected);
    closeToEditor();
  }
  function submit() {
    if (pane.kind !== "edit" || pane.saving) return;
    dispatch({ type: "submit" });
    if (!dirty(pane)) return;
    void updateSaved(pane.id, pane.draft).then((error) => dispatch(error ? { type: "failed", message: editErrorText(error) } : { type: "saved" }));
  }
  function onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    if (commandForKey(e.nativeEvent)?.id === "saved-queries") {
      e.preventDefault();
      if (!held()) close();
      return;
    }
    if (!listKeysLive(pane)) return;
    const box = searchRef.current;
    const searchHasSelection = document.activeElement === box && box?.selectionStart !== box?.selectionEnd;
    const action = savedKey(e.nativeEvent, searchHasSelection);
    if (!action) return;
    if (e.target instanceof HTMLElement) {
      if (e.target.closest('[data-testid="saved-sql"]')) return;
      if ((action === "primary" || action === "open") && e.target.closest('button:not([role="option"])')) return;
    }
    e.preventDefault();
    e.stopPropagation();
    act(action);
  }
  const count = text.trim()
    ? `${ranked.length.toLocaleString()} of ${items.length.toLocaleString()}`
    : `${items.length.toLocaleString()} saved ${items.length === 1 ? "query" : "queries"}`;
  return (
    <div className="flex min-h-0 flex-1 flex-col" onKeyDown={onKeyDown}>
      <div className="flex min-h-14 shrink-0 items-center gap-3 border-b border-control px-4">
        <BookmarkIcon size={18} className="shrink-0 text-muted" aria-hidden />
        <input
          ref={searchRef}
          data-testid="saved-input"
          autoFocus={pane.kind === "view"}
          inert={frozen}
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          value={text}
          onChange={(e) => search(e.target.value)}
          role="combobox"
          aria-expanded={true}
          aria-autocomplete="list"
          aria-label="Search saved queries"
          aria-controls="saved-list"
          aria-activedescendant={selectedId !== null ? `saved-item-${selectedId}` : undefined}
          placeholder="Search saved queries by name or SQL"
          className="h-12 min-w-0 flex-1 bg-transparent text-ui-14 text-fg outline-none placeholder:text-dim"
        />
        {controls(frozen)}
      </div>
      {items.length === 0 ? (
        <div data-testid="saved-empty" className="grid min-h-0 flex-1 place-items-center p-8 text-center">
          <div className="flex max-w-[360px] flex-col items-center gap-2">
            <BookmarkIcon size={28} className="text-faint" aria-hidden />
            <p className="text-ui-14 font-medium text-fg">No saved queries yet</p>
            <p className="text-ui-12 text-dim">Save the statement at your cursor with {mac ? "⌘⇧E" : "Ctrl+Shift+E"}, or press Save query in query history.</p>
          </div>
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-[35%_minmax(0,1fr)]">
          <div
            ref={listRef}
            id="saved-list"
            role="listbox"
            aria-label="Saved queries"
            data-testid="saved-list"
            tabIndex={0}
            inert={frozen}
            aria-activedescendant={selectedId !== null ? `saved-item-${selectedId}` : undefined}
            className={cx(
              "min-w-0 overflow-x-hidden overflow-y-auto border-r border-control p-1.5 outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-focus",
              frozen && "opacity-60",
            )}
          >
            {ranked.length === 0 && (
              <p role="status" data-testid="saved-no-match" className="px-3 py-8 text-center text-ui-13 text-dim">
                No saved queries match “{text.trim()}”.
              </p>
            )}
            {ranked.map(({ query, nameMatch }) => (
              <button
                key={query.id}
                type="button"
                role="option"
                aria-selected={query.id === selectedId}
                id={`saved-item-${query.id}`}
                data-testid="saved-item"
                data-saved-id={query.id}
                tabIndex={-1}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => select(query.id)}
                onDoubleClick={() => query.id === selectedId && act("primary")}
                className={cx(
                  "block w-full min-w-0 cursor-default rounded-lg px-2.5 py-2.5 text-left outline-none",
                  query.id === selectedId ? ACTIVE_OPTION : "hover:bg-hover",
                )}
              >
                <div className="flex min-w-0 items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-ui-13 font-medium text-fg" title={query.name}>
                    <Highlight text={query.name} ranges={nameMatch} />
                  </span>
                  {query.databaseKind !== activeKind && <DatabaseBadge databaseKind={query.databaseKind} />}
                </div>
                <div className="mt-1 min-w-0 truncate font-mono text-code-sm text-dim" title={query.excerpt}>
                  {query.excerpt || query.sql}
                </div>
              </button>
            ))}
          </div>
          {pane.kind === "edit" || pane.kind === "confirmDiscard" ? (
            <EditForm
              edit={pane.kind === "edit" ? pane : pane.edit}
              confirming={pane.kind === "confirmDiscard"}
              mod={mod}
              dispatch={dispatch}
              submit={submit}
            />
          ) : !selected ? (
            <div className="grid place-items-center text-ui-13 text-dim">Select a query to preview</div>
          ) : (
            <section aria-label="Saved query" data-testid="saved-preview" className="flex min-h-0 min-w-0 flex-col gap-4 p-6">
              <div className="min-w-0 shrink-0">
                <div className="flex min-w-0 items-center gap-2">
                  <h2 className="min-w-0 truncate text-ui-14 font-semibold text-fg" title={selected.name}>
                    {selected.name}
                  </h2>
                  {entry.kind === "existing" && entry.id === selected.id && (
                    <span role="status" data-testid="saved-notice" className="shrink-0 rounded bg-pill px-1.5 py-0.5 text-ui-11 text-dim">
                      Already saved
                    </span>
                  )}
                </div>
                <div className="mt-1.5 flex min-w-0 items-center gap-1.5 text-ui-11 text-dim">
                  <DatabaseBadge databaseKind={selected.databaseKind} />
                  <span className="truncate">
                    Saved {when(selected.createdAtMs, now)} · Last used {when(selected.usedAtMs, now)}
                  </span>
                </div>
              </div>
              <div className="relative min-h-0 flex-1 overflow-hidden rounded-lg border border-control bg-bg">
                <SqlText mode="view" text={selected.sql} databaseKind={selected.databaseKind} label="Saved SQL" testId="saved-sql" />
              </div>
              {copyError && (
                <p role="alert" className="text-ui-11 text-danger">
                  Could not copy. Select the SQL and copy it manually.
                </p>
              )}
              {pane.kind === "confirmDelete" ? (
                <div className="flex shrink-0 items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-ui-13 text-fg">Delete “{selected.name}”?</span>
                  <Button variant="ghost" data-testid="saved-delete-cancel" onClick={() => dispatch({ type: "cancel" })}>
                    Cancel
                  </Button>
                  <Button
                    variant="danger"
                    data-testid="saved-delete-confirm"
                    autoFocus
                    onClick={() => {
                      void deleteSaved(pane.id);
                      dispatch({ type: "confirm" });
                    }}
                  >
                    Delete
                  </Button>
                </div>
              ) : (
                <div className="flex shrink-0 flex-wrap items-center gap-2">
                  <Button
                    variant={insertable ? "primary" : "ghost"}
                    data-testid="saved-insert"
                    disabled={!insertable}
                    title={insertable ? "Insert into the active tab" : "Open a query tab to insert this query"}
                    onClick={() => act("primary")}
                  >
                    Insert at cursor{insertable && <Kbd tone="accent">↵</Kbd>}
                  </Button>
                  <Button variant={insertable ? "outline" : "primary"} data-testid="saved-open" onClick={() => act("open")}>
                    Open in new tab
                    <Kbd tone={insertable ? "surface" : "accent"} className={insertable ? "text-dim" : undefined}>
                      {insertable ? `${mod}↵` : "↵"}
                    </Kbd>
                  </Button>
                  <Button data-testid="saved-copy" onClick={() => act("copy")}>
                    <CopyIcon size={13} />
                    Copy<Kbd className="text-dim">{mod}C</Kbd>
                  </Button>
                  <span className="ml-auto flex gap-1">
                    <Button variant="ghost" data-testid="saved-edit" onClick={() => dispatch({ type: "edit", query: selected, focus: "sql" })}>
                      <PencilIcon size={13} />
                      Edit
                    </Button>
                    <Button variant="ghost" data-testid="saved-delete" onClick={() => dispatch({ type: "askDelete", id: selected.id })}>
                      <Trash2Icon size={13} />
                      Delete
                    </Button>
                  </span>
                </div>
              )}
            </section>
          )}
        </div>
      )}
      <div className="flex min-h-10 shrink-0 items-center gap-3 border-t border-control px-4 text-ui-11.5 text-dim">
        <span>
          <Kbd>↑↓</Kbd> Navigate
        </span>
        <span>
          <Kbd>↵</Kbd> {insertable ? "Insert" : "Open"}
        </span>
        <span>
          <Kbd>{mod}↵</Kbd> New tab
        </span>
        <span>
          <Kbd>{mod}C</Kbd> Copy
        </span>
        <span data-testid="saved-count" role="status" className="ml-auto shrink-0">
          {count}
        </span>
        <button
          type="button"
          data-testid="saved-save-current"
          disabled={!insertable || !listKeysLive(pane)}
          title={insertable ? "Save the selection, or the statement at the cursor" : "Open a query tab to save from it"}
          onClick={() => {
            close();
            void saveActiveQuery();
          }}
          className="flex shrink-0 items-center gap-1.5 rounded px-1 py-1 text-dim outline-none hover:text-fg focus-visible:ring-1 focus-visible:ring-focus disabled:opacity-40"
        >
          <BookmarkPlusIcon size={13} />
          Save current query<Kbd>{mac ? "⌘⇧E" : "Ctrl+Shift+E"}</Kbd>
        </button>
      </div>
    </div>
  );
}

function EditForm({
  edit,
  confirming,
  mod,
  dispatch,
  submit,
}: {
  edit: EditPane;
  confirming: boolean;
  mod: string;
  dispatch: (event: PaneEvent) => void;
  submit: () => void;
}) {
  const nameRef = useRef<HTMLInputElement>(null);
  const wasConfirming = useRef(confirming);
  useEffect(() => {
    if (edit.focus === "name") nameRef.current?.select();
  }, [edit.focus]);
  useEffect(() => {
    if (wasConfirming.current && !confirming) nameRef.current?.focus();
    wasConfirming.current = confirming;
  }, [confirming]);
  return (
    <section aria-label="Edit saved query" data-testid="saved-preview" className="flex min-h-0 min-w-0 flex-col gap-3 p-6">
      <div inert={confirming} className="flex min-h-0 flex-1 flex-col gap-3">
        <input
          ref={nameRef}
          data-testid="saved-name-input"
          aria-label="Name"
          autoFocus={edit.focus === "name"}
          spellCheck={false}
          value={edit.draft.name}
          placeholder="Named from the SQL when blank"
          onChange={(e) => dispatch({ type: "change", draft: { name: e.target.value } })}
          onKeyDown={(e) => {
            if ((e.key === "Enter" && !e.nativeEvent.isComposing) || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s")) {
              e.preventDefault();
              submit();
            }
          }}
          className={inputClass}
        />
        <div className="relative min-h-0 flex-1 overflow-hidden rounded-lg border border-control bg-bg">
          <SqlText
            mode="edit"
            initial={edit.base.sql}
            databaseKind={edit.databaseKind}
            label="SQL"
            testId="saved-sql-edit"
            autoFocus={edit.focus === "sql"}
            onChange={(sql) => dispatch({ type: "change", draft: { sql } })}
            onSubmit={submit}
          />
        </div>
      </div>
      {edit.error && (
        <p role="alert" data-testid="saved-edit-error" className="text-ui-11 text-danger">
          {edit.error}
        </p>
      )}
      {confirming ? (
        <div className="flex shrink-0 items-center gap-2">
          <span className="min-w-0 flex-1 truncate text-ui-13 text-fg">Discard your changes?</span>
          <Button variant="ghost" data-testid="saved-discard-keep" onClick={() => dispatch({ type: "cancel" })}>
            Keep editing
          </Button>
          <Button variant="danger" data-testid="saved-discard-confirm" autoFocus onClick={() => dispatch({ type: "confirm" })}>
            Discard
          </Button>
        </div>
      ) : (
        <div className="flex shrink-0 items-center gap-2">
          <Button variant="primary" data-testid="saved-edit-save" disabled={edit.saving} onClick={submit}>
            Save<Kbd tone="accent">{mod}S</Kbd>
          </Button>
          <Button variant="ghost" data-testid="saved-edit-cancel" disabled={edit.saving} onClick={() => dispatch({ type: "escape" })}>
            Cancel<Kbd className="text-dim">esc</Kbd>
          </Button>
        </div>
      )}
    </section>
  );
}
