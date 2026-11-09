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
import { Button, cx, IconButton, inputClass, Spinner } from "../../ui/primitives";
import { editView, probeDatabases } from "../connections/connectionActions";
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
    <Popover testId="picker-databases" trigger={trigger} title={model.title} subtitle={model.subtitle}>
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
  const [query, setQuery] = useState("");
  if (!state) return null;
  if (!catalog) {
    const failed = state.catalog.kind === "Failed" ? state.catalog.message : null;
    return (
      <Popover testId="picker-schemas" trigger={trigger} title={`Schemas in ${db.database}`} subtitle={failed ?? "Loading catalog"}>
        {!failed && (
          <div className="flex h-8 items-center gap-2 border-t border-control px-3 text-ui-12 text-muted">
            <Spinner /> Loading schemas
          </div>
        )}
      </Popover>
    );
  }
  const model = schemaPicker(state, catalog, query);
  return (
    <Popover testId="picker-schemas" trigger={trigger} title={model.title} subtitle={model.subtitle}>
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
function Popover({
  testId,
  trigger,
  title,
  subtitle,
  children,
}: {
  testId: string;
  trigger: HTMLElement;
  title: string;
  subtitle: string;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const close = usePicker((s) => s.close);

  const place = useCallback(() => {
    const el = ref.current;
    const row = trigger.closest<HTMLElement>('[role="treeitem"]');
    if (!el || !row) return;
    const box = pickerBox(row.getBoundingClientRect(), { width: window.innerWidth, height: window.innerHeight });
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
      className="motion-pop fixed z-30 flex flex-col overflow-hidden rounded-xl border border-control bg-raised shadow-[var(--shadow-popover)] outline-none"
    >
      <div className="relative shrink-0 px-3 pt-2.5 pb-2">
        <div title={title} className="truncate pr-7 text-ui-14 font-semibold text-fg">
          {title}
        </div>
        <div className="truncate text-ui-12 text-muted">{subtitle}</div>
        <IconButton data-testid="picker-close" aria-label="Close" onClick={close} className="absolute top-2 right-2 size-6">
          <XIcon size={15} />
        </IconButton>
      </div>
      {children}
    </div>,
    document.body,
  );
}

const ROW_HEIGHT = 32;

function Check({ checked, disabled, onChange, testId }: { checked: boolean; disabled?: boolean; onChange: (on: boolean) => void; testId: string }) {
  return (
    <span className="relative grid size-4 shrink-0 place-items-center">
      <input
        type="checkbox"
        data-testid={testId}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="peer size-4 appearance-none rounded-[4px] border border-dim checked:border-run checked:bg-run"
      />
      <CheckIcon size={12} strokeWidth={3} className="pointer-events-none absolute text-run-fg opacity-0 peer-checked:opacity-100" />
    </span>
  );
}

const QUICK_ACTIONS = [
  { preset: "All", label: "All", testId: "picker-all" },
  { preset: "DefaultOnly", label: "Default only", testId: "picker-default-only" },
  { preset: "None", label: "None", testId: "picker-none" },
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
      <div className="relative mx-3 shrink-0">
        <SearchIcon size={15} className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-muted" />
        <input
          data-testid="picker-filter"
          data-autofocus
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder={model.placeholder}
          className={cx(inputClass, "pl-8")}
        />
      </div>
      <div className="flex shrink-0 gap-1 px-1.5 py-1.5">
        {QUICK_ACTIONS.map((a) => (
          <button
            key={a.preset}
            type="button"
            data-testid={a.testId}
            onClick={() => onEdit(model.preset(a.preset))}
            className="h-7 rounded-md px-1.5 text-ui-13 text-muted hover:bg-hover hover:text-fg"
          >
            {a.label}
          </button>
        ))}
      </div>
      <div ref={listRef} className="min-h-0 shrink overflow-y-auto border-t border-control" style={{ maxHeight: 10 * ROW_HEIGHT }}>
        {model.items.length === 0 ? (
          <div className="flex h-8 items-center px-3 text-ui-13 text-muted">No matches</div>
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
                  className={cx("absolute inset-x-0 top-0 flex items-center gap-2.5 px-3", item.disabled ? "opacity-45" : "hover:bg-hover")}
                >
                  <Check
                    testId={`picker-check-${item.name}`}
                    checked={item.checked}
                    disabled={item.disabled}
                    onChange={(on) => onEdit(model.toggle(item.name, on))}
                  />
                  <span className="min-w-0 truncate text-ui-14 text-fg">{item.name}</span>
                  {item.isDefault && <span className="shrink-0 rounded-[4px] bg-pill px-1.5 py-0.5 text-ui-11 leading-none text-muted">default</span>}
                  <span title={item.trailingTitle} className="ml-auto shrink-0 pl-2 text-ui-12 text-muted tabular-nums">
                    {item.trailing}
                  </span>
                </label>
              );
            })}
          </div>
        )}
      </div>
      {model.footnote && <div className="shrink-0 px-3 pt-1.5 pb-2.5 text-ui-12 text-muted">{model.footnote}</div>}
      <div className="flex shrink-0 items-center justify-between gap-2 border-t border-control px-3 py-2.5">
        <label className="flex min-w-0 items-center gap-2.5 text-ui-13 text-muted">
          <Check testId="picker-system" checked={model.showSystem} onChange={(on) => onEdit(model.setShowSystem(on))} />
          <span title={model.systemLabel} className="truncate">
            {model.systemLabel}
          </span>
        </label>
        <Button data-testid="picker-done" onClick={close}>
          Done
        </Button>
      </div>
    </>
  );
}
