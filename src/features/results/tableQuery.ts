import { useEffect, useState } from "react";
import { create } from "zustand";
import { commands, type ColumnFilter, type FilterOp, type TabId, type TableOrderView, type TableQuery, type TableSortKey } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { sameDb } from "../../state/dbKey";
import { EMPTY_QUERY, isTableTab, useWorkspace, type Tab, type TableTab } from "../../state/workspace";
import { updateTableQuery } from "../editor/runActions";

export type SortKey = TableSortKey;

export const FILTER_OPS: readonly { op: FilterOp; label: string; takesValue: boolean }[] = [
  { op: "Eq", label: "=", takesValue: true },
  { op: "Ne", label: "≠", takesValue: true },
  { op: "Lt", label: "<", takesValue: true },
  { op: "Le", label: "≤", takesValue: true },
  { op: "Gt", label: ">", takesValue: true },
  { op: "Ge", label: "≥", takesValue: true },
  { op: "Contains", label: "contains", takesValue: true },
  { op: "StartsWith", label: "starts with", takesValue: true },
  { op: "IsNull", label: "is null", takesValue: false },
  { op: "NotNull", label: "is not null", takesValue: false },
];

export const takesValue = (op: FilterOp) => FILTER_OPS.find((o) => o.op === op)?.takesValue ?? true;

export function useTableOrder(tab: TableTab | null): TableSortKey[] {
  const db = tab?.console.db;
  const schema = tab?.table.schema;
  const table = tab?.table.table;
  const orderBy = tab?.table.query.orderBy;
  const catalogVersion = useCatalog((s) => viewFor(s.views, db)?.catalog.version);
  const [view, setView] = useState<{ key: string; value: TableOrderView } | null>(null);
  const key = JSON.stringify([db, schema, table, catalogVersion, orderBy]);
  useEffect(() => {
    let stale = false;
    if (db && schema !== undefined && table !== undefined && orderBy !== undefined)
      void commands.tableOrder(db, schema, table, orderBy, { kind: "Inspect" }).then((res) => {
        if (!stale && res.status === "ok") setView({ key, value: res.data });
      });
    return () => {
      stale = true;
    };
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- `key` encodes every input, and `db` is a new object on every render
  }, [key]);
  return view?.key === key ? (view.value.keys ?? []) : [];
}

type OrderQueue = { query: TableQuery; tail: Promise<void> };
const orderQueues = new Map<TabId, OrderQueue>();

export function cycleTableOrder(tab: TableTab, column: string, additive: boolean): Promise<void> {
  const queue = orderQueues.get(tab.id) ?? { query: tab.table.query, tail: Promise.resolve() };
  const unchanged = (t: Tab | undefined): t is TableTab =>
    isTableTab(t) &&
    sameDb(t.console.db, tab.console.db) &&
    t.table.schema === tab.table.schema &&
    t.table.table === tab.table.table &&
    t.table.query === queue.query;
  const write = queue.tail.then(async () => {
    const current = useWorkspace.getState().tabs[tab.id];
    if (!unchanged(current)) return;
    const res = await commands.tableOrder(current.console.db, current.table.schema, current.table.table, queue.query.orderBy, {
      kind: "Cycle",
      column,
      additive,
    });
    if (!unchanged(useWorkspace.getState().tabs[tab.id])) return;
    if (res.status === "error") return useWorkspace.getState().notify(res.error);
    if (res.data.sql !== queue.query.orderBy) {
      queue.query = { ...queue.query, orderBy: res.data.sql };
      updateTableQuery(tab.id, () => queue.query);
    }
  });
  const tail = write.catch(() => {});
  queue.tail = tail;
  orderQueues.set(tab.id, queue);
  void tail.then(() => {
    if (orderQueues.get(tab.id)?.tail === tail) orderQueues.delete(tab.id);
  });
  return write;
}

export function filterForValue(column: string, value: string | null, negate: boolean): ColumnFilter {
  if (value === null) return { column, op: negate ? "NotNull" : "IsNull", value: "" };
  return { column, op: negate ? "Ne" : "Eq", value };
}

export function referencedRows(refColumns: string[], values: string[]): TableQuery {
  return { ...EMPTY_QUERY, filters: refColumns.map((column, i) => ({ column, op: "Eq", value: values[i]! })) };
}

export type FilterDraft = ColumnFilter & { id: number };

let nextDraftId = 1;

export const useFilterDrafts = create<{
  drafts: Record<TabId, FilterDraft[]>;
  add(tab: TabId, column: string): void;
  remove(tab: TabId, id: number): void;
}>((set) => ({
  drafts: {},
  add: (tab, column) => set((s) => ({ drafts: { ...s.drafts, [tab]: [...(s.drafts[tab] ?? []), { id: nextDraftId++, column, op: "Eq", value: "" }] } })),
  remove: (tab, id) => set((s) => ({ drafts: { ...s.drafts, [tab]: (s.drafts[tab] ?? []).filter((d) => d.id !== id) } })),
}));
