import { create } from "zustand";
import type { CellWrite, ChangeRef, RowCheck, TabId, TableChanges } from "../ipc/bindings";

export type CellValue = string | null;
type RowKey = string;
export type RowId = readonly Readonly<CellWrite>[];

export type NewRow = { id: number; values: Record<string, CellValue> };

export type TableEdits = {
  inserts: NewRow[];
  updates: Record<RowKey, { key: RowId; values: Record<string, CellValue> }>;
  /** Each edited cell's stored value from when it was first staged. */
  originals?: Record<RowKey, Record<string, CellValue>>;
  deletes: Record<RowKey, RowId>;
  /** A SQL Server rowversion table's staged rows, with the rowversion each showed when first staged. */
  versions?: Record<RowKey, CellValue>;
  failedAt: { insert: number } | { key: RowKey } | null;
};

export const NO_EDITS: TableEdits = { inserts: [], updates: {}, deletes: {}, failedAt: null };

export const rowKey = (cells: RowId): RowKey => JSON.stringify(cells.map(({ column, value }) => [column, value]));
const copyKey = (cells: RowId): CellWrite[] => cells.map(({ column, value }) => ({ column, value }));

export function pendingCount(e: TableEdits): number {
  return e.inserts.length + Object.keys(e.updates).filter((k) => !(k in e.deletes)).length + Object.keys(e.deletes).length;
}

export function addRow(e: TableEdits, id: number): TableEdits {
  return { ...e, inserts: [...e.inserts, { id, values: {} }] };
}

export function setNewCell(e: TableEdits, id: number, column: string, value: CellValue): TableEdits {
  return { ...e, inserts: e.inserts.map((r) => (r.id === id ? { ...r, values: { ...r.values, [column]: value } } : r)) };
}

export function resetNewCell(e: TableEdits, id: number, column: string): TableEdits {
  return {
    ...e,
    inserts: e.inserts.map((r) => {
      if (r.id !== id) return r;
      const { [column]: _, ...rest } = r.values;
      return { ...r, values: rest };
    }),
  };
}

export function removeNewRows(e: TableEdits, ids: number[]): TableEdits {
  return { ...e, inserts: e.inserts.filter((r) => !ids.includes(r.id)) };
}

export function setCell(e: TableEdits, row: RowId, column: string, value: CellValue, stored: CellValue): TableEdits {
  const key = rowKey(row);
  const current = e.updates[key]?.values ?? {};
  const { [column]: _, ...rest } = current;
  const values = value === stored ? rest : { ...rest, [column]: value };
  const { [key]: __, ...others } = e.updates;
  const { [column]: original = stored, ...kept } = e.originals?.[key] ?? {};
  const originals = { ...e.originals, [key]: value === stored ? kept : { ...kept, [column]: original } };
  return { ...e, originals, updates: Object.keys(values).length ? { ...others, [key]: { key: e.updates[key]?.key ?? copyKey(row), values } } : others };
}

/** Keeps the rowversion `version` showed for `row` the first time one of its edits is staged. */
export function keepVersion(e: TableEdits, row: RowId, version: CellValue): TableEdits {
  const key = rowKey(row);
  return key in (e.versions ?? {}) ? e : { ...e, versions: { ...e.versions, [key]: version } };
}

export function markDeleted(e: TableEdits, rows: RowId[], on: boolean): TableEdits {
  const deletes = { ...e.deletes };
  for (const r of rows) {
    const key = rowKey(r);
    if (on) deletes[key] ??= copyKey(r);
    else delete deletes[key];
  }
  return { ...e, deletes };
}

export function toggleDeletes(e: TableEdits, rows: RowId[]): TableEdits {
  return markDeleted(e, rows, !rows.every((r) => rowKey(r) in e.deletes));
}

const entries = (values: Record<string, CellValue>): CellWrite[] => Object.entries(values).map(([column, value]) => ({ column, value }));

/**
 * `rowCheck` is the SQL Server table's way of proving a row unchanged: its rowversion, or the stored value of each edited
 * cell. PostgreSQL has none and sends no `expect`.
 */
export function toChanges(
  e: TableEdits,
  schema: string,
  table: string,
  rowCheck: RowCheck | null = null,
): { changes: TableChanges; rowOf: (ref: ChangeRef) => TableEdits["failedAt"] } {
  const deleteKeys = Object.keys(e.deletes);
  const updateKeys = Object.keys(e.updates).filter((k) => !(k in e.deletes));
  const version = (k: RowKey): CellWrite[] => (rowCheck?.kind === "Rowversion" ? [{ column: rowCheck.column, value: e.versions?.[k] ?? null }] : []);
  const originals = (k: RowKey): CellWrite[] =>
    rowCheck?.kind === "Values" ? Object.keys(e.updates[k]!.values).map((column) => ({ column, value: e.originals?.[k]?.[column] ?? null })) : version(k);
  const changes: TableChanges = {
    schema,
    table,
    inserts: e.inserts.map((r) => entries(r.values)),
    updates: updateKeys.map((k) => ({ key: copyKey(e.updates[k]!.key), set: entries(e.updates[k]!.values), expect: originals(k) })),
    deletes: deleteKeys.map((k) => ({ key: copyKey(e.deletes[k]!), expect: version(k) })),
  };
  const rowOf = (ref: ChangeRef): TableEdits["failedAt"] => {
    if (ref.kind === "Insert") return { insert: e.inserts[ref.index]!.id };
    return { key: (ref.kind === "Update" ? updateKeys : deleteKeys)[ref.index]! };
  };
  return { changes, rowOf };
}

let nextRowId = 1;

export const useTableEdits = create<{
  byTab: Record<TabId, TableEdits>;
  /** Tabs whose submit is in flight; their edits can't change until it settles. */
  submitting: Record<TabId, true>;
  /** False when a submit for the tab is in flight and the edit was dropped. */
  edit(tab: TabId, f: (e: TableEdits) => TableEdits): boolean;
  addRow(tab: TabId): number | null;
  clear(tab: TabId): void;
  /** False when a submit for the tab is already in flight. */
  beginSubmit(tab: TabId): boolean;
  endSubmit(tab: TabId): void;
}>((set, get) => ({
  byTab: {},
  submitting: {},
  edit: (tab, f) => {
    if (get().submitting[tab]) return false;
    set((s) => ({ byTab: { ...s.byTab, [tab]: { ...f(s.byTab[tab] ?? NO_EDITS), failedAt: null } } }));
    return true;
  },
  addRow: (tab) => {
    if (get().submitting[tab]) return null;
    const id = nextRowId++;
    set((s) => ({ byTab: { ...s.byTab, [tab]: addRow(s.byTab[tab] ?? NO_EDITS, id) } }));
    return id;
  },
  beginSubmit: (tab) => {
    if (get().submitting[tab]) return false;
    set((s) => ({ submitting: { ...s.submitting, [tab]: true } }));
    return true;
  },
  endSubmit: (tab) =>
    set((s) => {
      const { [tab]: _, ...rest } = s.submitting;
      return { submitting: rest };
    }),
  clear: (tab) =>
    set((s) => {
      const { [tab]: _, ...rest } = s.byTab;
      return { byTab: rest };
    }),
}));
