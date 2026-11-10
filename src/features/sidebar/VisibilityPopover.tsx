import { useVirtualizer } from "@tanstack/react-virtual";
import { CheckIcon, SearchIcon, XIcon } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { create } from "zustand";
import type { ConnectionId, DatabaseRef, ViewEdit } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { instanceOf, useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { Button, cx, IconButton, Spinner, Switch } from "../../ui/primitives";
import { editView, probeDatabases, refreshCatalog } from "../connections/connectionActions";
import { countText, databasePicker, namesToProbe, pickerBox, schemaPicker, type PickerItem, type PickerModel, type ShownCount } from "./pickers";

export type PickerTarget = { kind: "databases"; id: ConnectionId } | { kind: "schemas"; db: DatabaseRef };

function targetKey(t: PickerTarget): string {
  return t.kind === "databases" ? `databases:${t.id}` : `schemas:${dbKey(t.db)}`;
}

type OpenPicker = { key: string; target: PickerTarget; trigger: HTMLElement };

/** One picker at a time. It lives outside the tree's rows so it stays open while snapshots re-render them. */
const usePicker = create<{ open: OpenPicker | null; toggle(target: PickerTarget, trigger: HTMLElement): void; close(): void }>()((set, get) => ({
  open: null,
  toggle(target, trigger) {
    const key = targetKey(target);
    set({ open: get().open?.key === key ? null : { key, target, trigger } });
  },
  close: () => set({ open: null }),
}));

export function useIsPickerOpen(target: PickerTarget): boolean {
  const key = targetKey(target);
  return usePicker((s) => s.open?.key === key);
}

/** The `shown/total` chip on a connection or database row. It opens its picker and leaves the row's own click alone. */
/** `label` replaces the `shown/total` text, and gives the pill a filled look for the sidebar's database list. */
export function PickerPill({
  target,
  count,
  active,
  testId,
  label,
}: {
  target: PickerTarget;
  count: ShownCount;
  active: boolean;
  testId: string;
  label?: string;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      aria-label={target.kind === "schemas" ? "Choose schemas" : "Choose databases"}
      aria-haspopup="dialog"
      aria-expanded={active}
      onClick={(e) => {
        e.stopPropagation();
        usePicker.getState().toggle(target, e.currentTarget);
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      className={cx(
        "inline-flex shrink-0 items-center leading-none tabular-nums",
        label ? "ml-2 h-[18px] rounded px-1.5 text-ui-10.5 font-medium" : "-mr-1.5 ml-1 h-5 rounded-md px-1.5 text-ui-11.5",
        count.warn ? "text-amber" : "text-muted",
        active ? "bg-control" : label ? "bg-control/70 hover:bg-control" : "hover:bg-control",
      )}
    >
      {label ?? countText(count)}
    </button>
  );
}

export function PickerHost() {
  const open = usePicker((s) => s.open);
  if (!open) return null;
  return open.target.kind === "databases" ? (
    <DatabasesPicker key={open.key} id={open.target.id} trigger={open.trigger} />
  ) : (
    <SchemasPicker key={open.key} db={open.target.db} trigger={open.trigger} />
  );
}

/** The pill unmounts when its row goes (a disconnect, a hidden database, a collapsed parent), and its picker goes with it. */
function useCloseWhenGone(trigger: HTMLElement) {
  useLayoutEffect(() => {
    if (!trigger.isConnected) usePicker.getState().close();
  });
}

function DatabasesPicker({ id, trigger }: { id: ConnectionId; trigger: HTMLElement }) {
  useCloseWhenGone(trigger);
  const config = useWorkspace((s) => s.configs[id]);
  const instance = useLive((s) => instanceOf(s.connections[id]));
  const [query, setQuery] = useState("");
  if (!config || !instance) return null;
  const model = databasePicker(config, instance, query);
  return (
    <Popover testId="picker-databases" trigger={trigger} title={model.title} context={model.context}>
      <PickerBody
        model={model}
        query={query}
        onQuery={setQuery}
        onEdit={(edit) => void editView(id, edit)}
        probe={{ pick: (visible) => namesToProbe(instance, visible), send: (names) => probeDatabases(id, names) }}
      />
    </Popover>
  );
}

function SchemasPicker({ db, trigger }: { db: DatabaseRef; trigger: HTMLElement }) {
  useCloseWhenGone(trigger);
  const state = useLive((s) => instanceOf(s.connections[db.connectionId])?.databases.find((d) => d.name === db.database));
  const catalog = useCatalog((s) => viewFor(s.views, db)?.catalog);
  const connectionName = useWorkspace((s) => s.configs[db.connectionId]?.name ?? "");
  const [query, setQuery] = useState("");
  if (!state) return null;
  const failure =
    state.catalog.kind === "Failed" ? (
      <div role="alert" className="flex shrink-0 items-center gap-2 text-ui-12 text-danger">
        <span className="min-w-0 flex-1 break-words">{state.catalog.message}</span>
        <Button onClick={() => void refreshCatalog(db)}>Retry</Button>
      </div>
    ) : null;
  if (!catalog) {
    return (
      <Popover testId="picker-schemas" trigger={trigger} title="Schemas in sidebar" context={`${db.database} · ${connectionName}`}>
        {failure ?? (
          <div className="flex h-8 items-center gap-2 border-t border-control px-3 text-ui-12 text-muted">
            <Spinner /> Loading schemas
          </div>
        )}
      </Popover>
    );
  }
  const model = schemaPicker(state, catalog, query, connectionName);
  return (
    <Popover testId="picker-schemas" trigger={trigger} title={model.title} context={model.context}>
      {failure}
      <PickerBody model={model} query={query} onQuery={setQuery} onEdit={(edit) => void editView(db.connectionId, edit)} />
    </Popover>
  );
}

const FOCUSABLE = 'input:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Tab wraps inside the dialog, so the keyboard cannot wander into the page behind it. */
function wrapTab(e: ReactKeyboardEvent<HTMLElement>) {
  if (e.key !== "Tab") return;
  const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>(FOCUSABLE));
  const first = items[0];
  const last = items[items.length - 1];
  if (!first || !last) return;
  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
}

/**
 * Placed by `pickerBox` beside the trigger's row. Escape and a click outside close it. It holds keyboard focus while open
 * and gives it back to the pill when it closes.
 */
function Popover({ testId, trigger, title, context, children }: { testId: string; trigger: HTMLElement; title: string; context: string; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const close = usePicker((s) => s.close);

  const place = useCallback(() => {
    const el = ref.current;
    const row = trigger.closest<HTMLElement>("[data-picker-anchor]") ?? trigger.closest<HTMLElement>('[role="treeitem"]') ?? trigger;
    if (!el) return;
    const list = el.querySelector<HTMLElement>("[data-picker-list]");
    const contentHeight = el.scrollHeight + (list ? Math.max(0, list.scrollHeight - list.clientHeight) : 0);
    const box = pickerBox(row.getBoundingClientRect(), { width: window.innerWidth, height: window.innerHeight }, contentHeight);
    el.style.top = "top" in box.anchor ? `${box.anchor.top}px` : "";
    el.style.bottom = "bottom" in box.anchor ? `${box.anchor.bottom}px` : "";
    el.style.left = `${box.left}px`;
    el.style.width = `${box.width}px`;
    el.style.maxHeight = `${box.maxHeight}px`;
  }, [trigger]);

  // Every render: a snapshot can move the row.
  useLayoutEffect(place);

  // The filter takes focus; the loading view has none, so the dialog itself does.
  useEffect(() => {
    const el = ref.current;
    if (el) (el.querySelector<HTMLElement>("[data-autofocus]") ?? el).focus();
    return () => {
      if (trigger.isConnected) trigger.focus();
    };
  }, [trigger]);

  useEffect(() => {
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (!ref.current?.contains(target) && !trigger.contains(target)) close();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      close();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [trigger, close, place]);

  return createPortal(
    <div
      ref={ref}
      data-testid={testId}
      role="dialog"
      aria-label={title}
      tabIndex={-1}
      onKeyDown={wrapTab}
      className="motion-pop fixed z-30 flex w-[360px] max-w-[calc(100vw-16px)] flex-col gap-2.5 overflow-y-auto rounded-[10px] border border-border p-4 bg-raised shadow-[var(--shadow-popover)] outline-none"
    >
      <div className="relative shrink-0">
        <div title={title} className="truncate pr-7 text-ui-14 font-semibold text-fg">
          {title}
        </div>

        <IconButton data-testid="picker-close" aria-label="Close" onClick={close} className="absolute -top-0.5 -right-0.5 size-6">
          <XIcon size={15} />
        </IconButton>
      </div>
      <div className="shrink-0 truncate text-ui-11 text-dim">{context}</div>
      {children}
    </div>,
    document.body,
  );
}

const ROW_HEIGHT = 38;

function Check({ checked, disabled, onChange, testId }: { checked: boolean; disabled?: boolean; onChange: (on: boolean) => void; testId: string }) {
  return (
    <span className="relative grid size-4 shrink-0 place-items-center">
      <input
        type="checkbox"
        data-testid={testId}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="peer size-4 appearance-none rounded-[3px] border border-control checked:border-run checked:bg-run"
      />
      <CheckIcon size={12} strokeWidth={3} className="pointer-events-none absolute text-run-fg opacity-0 peer-checked:opacity-100" />
    </span>
  );
}

const QUICK_ACTIONS = [
  { preset: "All", label: "All", testId: "picker-all", title: "Show all current and future non-system items. Select system items individually." },
  {
    preset: "DefaultOnly",
    label: "Default only",
    testId: "picker-default-only",
    title: "Follow the default as it changes. Hide all other items.",
  },
  { preset: "None", label: "None", testId: "picker-none", title: "Hide all items, including system items. Open notebooks and queries stay open." },
] as const;

/** `probe` asks the engine for the schema counts of the rows on screen; only the database picker has one. */
type Probe = { pick(visible: readonly PickerItem[]): string[]; send(names: string[]): void };

function PickerBody({
  model,
  query,
  onQuery,
  onEdit,
  probe,
}: {
  model: PickerModel;
  query: string;
  onQuery: (q: string) => void;
  onEdit: (edit: ViewEdit) => void;
  probe?: Probe;
}) {
  const close = usePicker((s) => s.close);
  const listRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({ count: model.items.length, getScrollElement: () => listRef.current, estimateSize: () => ROW_HEIGHT, overscan: 4 });
  const rows = virtualizer.getVirtualItems();

  const unprobed = probe ? probe.pick(rows.flatMap((r) => model.items[r.index] ?? [])) : [];
  const unprobedKey = unprobed.join("\u0000");
  useEffect(() => {
    if (unprobed.length > 0) probe?.send(unprobed);
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- `unprobedKey` stands in for `probe` and `unprobed`, which are new on every render
  }, [unprobedKey]);

  return (
    <>
      <div className="relative shrink-0">
        <SearchIcon size={15} className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-muted" />
        <input
          data-testid="picker-filter"
          aria-label={model.placeholder}
          data-autofocus
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder={model.placeholder}
          className="h-[39px] w-full rounded-[5px] bg-hover pr-2 pl-8 text-ui-12 text-fg placeholder:text-dim"
        />
      </div>
      <div className="flex shrink-0 flex-wrap items-center gap-2">
        <span className="mr-auto text-ui-11 text-muted">
          {model.count.shown} of {model.count.total} shown
        </span>
        {QUICK_ACTIONS.map((a) => (
          <button
            key={a.preset}
            type="button"
            data-testid={a.testId}
            title={a.title}
            onClick={() => onEdit(model.preset(a.preset))}
            className="rounded text-ui-11 font-medium text-keyword hover:bg-hover"
          >
            {a.label}
          </button>
        ))}
      </div>
      <div className="h-px shrink-0 bg-border" />
      <div ref={listRef} data-picker-list className="min-h-[38px] shrink overflow-y-auto" style={{ maxHeight: 10 * ROW_HEIGHT }}>
        {model.items.length === 0 ? (
          <div className="flex h-8 items-center px-3 text-ui-13 text-muted">{query.trim() ? "No matches" : "No items available"}</div>
        ) : (
          <div className="relative" style={{ height: virtualizer.getTotalSize() }}>
            {rows.map((r) => {
              const item = model.items[r.index]!;
              return (
                <label
                  key={item.name}
                  data-testid={`picker-row-${item.name}`}
                  title={item.disabled ? item.trailingTitle : undefined}
                  style={{ height: ROW_HEIGHT, transform: `translateY(${r.start}px)` }}
                  className={cx("absolute inset-x-0 top-0 flex items-center gap-2.5 rounded-[5px] px-2", item.disabled ? "opacity-65" : "hover:bg-hover")}
                >
                  <Check
                    testId={`picker-check-${item.name}`}
                    checked={item.checked}
                    disabled={item.disabled}
                    onChange={(on) => onEdit(model.toggle(item.name, on))}
                  />
                  <span className="min-w-0 truncate text-ui-12 text-fg">{item.name}</span>
                  <span title={item.trailingTitle} className={cx("ml-auto shrink-0 pl-2 text-ui-10 tabular-nums", item.disabled ? "text-danger" : "text-dim")}>
                    {item.isDefault && "Default · "}
                    {item.trailing}
                  </span>
                </label>
              );
            })}
          </div>
        )}
      </div>
      <div className="h-px shrink-0 bg-border" />
      <div className="flex shrink-0 items-center justify-between gap-2 text-ui-12 text-muted">
        <span>{model.systemLabel}</span>
        <Switch testId="picker-system" label={model.systemLabel} checked={model.showSystem} onCheckedChange={(on) => onEdit(model.setShowSystem(on))} />
      </div>
      <p className="shrink-0 text-ui-11 leading-[17px] text-dim">{model.footnote}</p>
      <div className="flex shrink-0 items-center justify-between gap-2">
        <span className="text-ui-10 text-dim">Sidebar visibility only</span>
        <Button variant="primary" data-testid="picker-done" onClick={close} className="h-[29px] rounded-[5px] px-1.5 text-ui-11">
          Done
        </Button>
      </div>
    </>
  );
}
