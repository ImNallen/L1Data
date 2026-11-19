import { expect, it } from "vitest";
import {
  addRow,
  dropStale,
  keepVersion,
  markDeleted,
  markStale,
  NO_EDITS,
  pendingCount,
  removeNewRows,
  resetNewCell,
  rowKey,
  setCell,
  setNewCell,
  toChanges,
  toggleDeletes,
  useTableEdits,
  type RowId,
} from "./tableEdits";

const row = (value: string | null): RowId => [{ column: "id", value }];
const T = { db: { connectionId: "c1", database: "shop" }, schema: "public", table: "t" };

it("turns pending edits into engine changes keyed by primary key", () => {
  let e = addRow(NO_EDITS, 7);
  e = setNewCell(e, 7, "name", "d");
  e = setNewCell(e, 7, "note", null);
  e = setCell(e, row("2"), "name", "b2", "b");
  e = toggleDeletes(e, [row("1")]);

  const { changes, rowOf } = toChanges(e, "public", "customers");
  expect(changes).toEqual({
    schema: "public",
    table: "customers",
    inserts: [
      [
        { column: "name", value: "d" },
        { column: "note", value: null },
      ],
    ],
    updates: [{ key: [{ column: "id", value: "2" }], set: [{ column: "name", value: "b2" }], expect: [] }],
    deletes: [{ key: [{ column: "id", value: "1" }], expect: [] }],
  });
  expect(pendingCount(e)).toBe(3);
  expect(rowOf({ kind: "Insert", index: 0 })).toEqual({ insert: 7 });
  expect(rowOf({ kind: "Update", index: 0 })).toEqual({ key: rowKey(row("2")) });
  expect(rowOf({ kind: "Delete", index: 0 })).toEqual({ key: rowKey(row("1")) });
});

it("drops an update set back to the stored value, and a new cell set back to its default", () => {
  let e = setCell(NO_EDITS, row("2"), "name", "b2", "b");
  e = setCell(e, row("2"), "name", "b", "b");
  expect(e.updates).toEqual({});

  e = setNewCell(addRow(NO_EDITS, 1), 1, "name", "x");
  e = resetNewCell(e, 1, "name");
  expect(toChanges(e, "s", "t").changes.inserts).toEqual([[]]);
});

it("deletes win over updates to the same row, and toggling twice restores it", () => {
  let e = setCell(NO_EDITS, row("2"), "name", "b2", "b");
  e = toggleDeletes(e, [row("2")]);
  expect(toChanges(e, "s", "t").changes.updates).toEqual([]);
  expect(pendingCount(e)).toBe(1);

  e = toggleDeletes(e, [row("2")]);
  expect(e.deletes).toEqual({});
  expect(pendingCount(e)).toBe(1);
});

it("marks a mixed selection deleted before it unmarks anything", () => {
  const e = toggleDeletes(toggleDeletes(NO_EDITS, [row("1")]), [row("1"), row("2")]);
  expect(Object.keys(e.deletes).sort()).toEqual([rowKey(row("1")), rowKey(row("2"))]);
});

it("keys composite primary keys in column order and removes new rows by id", () => {
  const key = [
    { column: "order_id", value: "7" },
    { column: "sku", value: "A-1" },
  ];
  const e = toggleDeletes(removeNewRows(addRow(addRow(NO_EDITS, 1), 2), [1]), [key]);
  expect(e.inserts.map((r) => r.id)).toEqual([2]);
  expect(toChanges(e, "s", "t").changes.deletes).toEqual([
    {
      key: [
        { column: "order_id", value: "7" },
        { column: "sku", value: "A-1" },
      ],
      expect: [],
    },
  ]);
});

it("holds a tab's edits still while its submit is in flight", () => {
  const store = useTableEdits.getState();
  store.edit("t", T, (e) => setNewCell(addRow(e, 1), 1, "name", "a"));
  expect(store.beginSubmit("t")).toBe(true);
  expect(store.beginSubmit("t")).toBe(false);

  store.edit("t", T, (e) => toggleDeletes(e, [row("9")]));
  expect(store.addRow("t", T)).toBeNull();
  expect(useTableEdits.getState().byTab["t"]).toEqual({ ...addRow(NO_EDITS, 1), table: T, inserts: [{ id: 1, values: { name: "a" } }], failedAt: null });

  store.endSubmit("t");
  store.edit("t", T, (e) => toggleDeletes(e, [row("9")]));
  expect(pendingCount(useTableEdits.getState().byTab["t"]!)).toBe(2);
});

it("keeps a tab's pending edits to one table until they clear, so a result of another table cannot stage beside them", () => {
  const store = useTableEdits.getState();
  const other = { ...T, table: "other" };
  expect(store.edit("v", T, (e) => setCell(e, row("1"), "name", "x", "y"))).toBe(true);
  expect(store.edit("v", other, (e) => setCell(e, row("1"), "name", "z", "y"))).toBe(false);
  expect(store.addRow("v", other)).toBeNull();
  expect(useTableEdits.getState().byTab["v"]?.updates).toEqual({ [rowKey(row("1"))]: { key: row("1"), values: { name: "x" } } });
  store.edit("v", T, (e) => setCell(e, row("1"), "name", "y", "y"));
  expect(store.edit("v", other, (e) => setCell(e, row("1"), "name", "z", "y"))).toBe(true);
  expect(useTableEdits.getState().byTab["v"]?.table).toEqual(other);
});

it("marks or unmarks rows for deletion the same way however often it runs", () => {
  const twice = markDeleted(markDeleted(NO_EDITS, [row("1"), row("2")], true), [row("1"), row("2")], true);
  expect(Object.keys(twice.deletes).sort()).toEqual([rowKey(row("1")), rowKey(row("2"))]);
  expect(markDeleted(twice, [row("1")], false).deletes).toEqual({ [rowKey(row("2"))]: row("2") });
});

it("tells the caller when an edit was refused during a submit", () => {
  const store = useTableEdits.getState();
  expect(store.beginSubmit("u")).toBe(true);
  expect(store.edit("u", T, (e) => markDeleted(e, [row("1")], true))).toBe(false);
  store.endSubmit("u");
  expect(store.edit("u", T, (e) => markDeleted(e, [row("1")], true))).toBe(true);
});

it("keeps equal key values under different primary-key names separate", () => {
  const alternate = [{ column: "alternate", value: "1" }];
  const updated = setCell(NO_EDITS, row("1"), "note", "edited", "original");
  const edits = markDeleted(updated, [alternate], true);
  expect(pendingCount(edits)).toBe(2);
  expect(toChanges(edits, "public", "t").changes).toEqual({
    schema: "public",
    table: "t",
    inserts: [],
    updates: [{ key: row("1"), set: [{ column: "note", value: "edited" }], expect: [] }],
    deletes: [{ key: alternate, expect: [] }],
  });
});

it.each(["update", "delete"])("captures an immutable named original key for a staged %s", (change) => {
  const cell = { column: "id", value: "1" };
  const selected = [cell];
  const edits = change === "update" ? setCell(NO_EDITS, selected, "note", "edited", "original") : markDeleted(NO_EDITS, [selected], true);
  cell.column = "alternate";
  cell.value = "2";
  const submitted = toChanges(edits, "public", "t").changes;
  const keys = change === "update" ? submitted.updates.map((u) => u.key) : submitted.deletes.map((d) => d.key);
  expect(keys).toEqual([row("1")]);
  keys[0]![0]!.column = "mutated request";
  expect(toChanges(edits, "public", "t").changes).toMatchObject(change === "update" ? { updates: [{ key: row("1") }] } : { deletes: [{ key: row("1") }] });
});

it("keeps the stored identity when the user edits a primary-key value", () => {
  const renamed = setCell(NO_EDITS, row("1"), "id", "2", "1");
  const edits = setCell(renamed, row("1"), "note", "edited", "original");
  expect(toChanges(edits, "public", "t").changes.updates).toEqual([
    {
      key: row("1"),
      set: [
        { column: "id", value: "2" },
        { column: "note", value: "edited" },
      ],
      expect: [],
    },
  ]);
});

it("sends a SQL Server row as the page showed it: each edited cell's first stored value, or the rowversion", () => {
  let e = setCell(NO_EDITS, row("1"), "name", "x", "a");
  e = setCell(e, row("1"), "name", "y", "x");
  e = keepVersion(e, row("1"), "0x01");
  e = keepVersion(e, row("1"), "0x02");
  e = keepVersion(markDeleted(e, [row("2")], true), row("2"), "0x03");
  expect(toChanges(e, "dbo", "t", { kind: "Values" }).changes).toMatchObject({
    updates: [{ key: row("1"), set: [{ column: "name", value: "y" }], expect: [{ column: "name", value: "a" }] }],
    deletes: [{ key: row("2"), expect: [] }],
  });
  expect(toChanges(e, "dbo", "t", { kind: "Rowversion", column: "stamp" }).changes).toMatchObject({
    updates: [{ expect: [{ column: "stamp", value: "0x01" }] }],
    deletes: [{ expect: [{ column: "stamp", value: "0x03" }] }],
  });
  expect(setCell(e, row("1"), "name", "a", "a").updates).toEqual({});
});

it.each([
  { check: { kind: "Values" } as const, expect: [{ column: "name", value: "b" }] },
  { check: { kind: "Rowversion", column: "stamp" } as const, expect: [{ column: "stamp", value: "0x02" }] },
])("records the page's current values when a row is staged again after its edits are undone ($check.kind)", ({ check, expect: sent }) => {
  let e = keepVersion(setCell(NO_EDITS, row("1"), "name", "x", "a"), row("1"), "0x01");
  e = keepVersion(setCell(e, row("1"), "name", "a", "a"), row("1"), "0x01");
  expect(e.originals?.[rowKey(row("1"))]).toBeUndefined();
  expect(e.versions?.[rowKey(row("1"))]).toBeUndefined();
  e = keepVersion(setCell(e, row("1"), "name", "x", "b"), row("1"), "0x02");
  expect(toChanges(e, "dbo", "t", check).changes.updates).toEqual([{ key: row("1"), set: [{ column: "name", value: "x" }], expect: sent }]);
});

it("forgets a restored delete's rowversion, and keeps it while the row still has an update", () => {
  let e = keepVersion(markDeleted(NO_EDITS, [row("1")], true), row("1"), "0x01");
  e = markDeleted(e, [row("1")], false);
  expect(e.versions).toEqual({});
  e = keepVersion(setCell(e, row("1"), "name", "x", "a"), row("1"), "0x02");
  e = markDeleted(keepVersion(markDeleted(e, [row("1")], true), row("1"), "0x03"), [row("1")], false);
  expect(e.versions).toEqual({ [rowKey(row("1"))]: "0x02" });
});

it("drops only a stale row's edits, so it can be staged again against the reloaded page", () => {
  let e = keepVersion(setCell(NO_EDITS, row("1"), "name", "x", "a"), row("1"), "0x01");
  e = keepVersion(setCell(e, row("2"), "name", "y", "b"), row("2"), "0x01");
  e = keepVersion(markDeleted(e, [row("3")], true), row("3"), "0x01");
  e = markStale(markStale(e, rowKey(row("1"))), rowKey(row("3")));
  e = dropStale(e);
  expect(Object.keys(e.updates)).toEqual([rowKey(row("2"))]);
  expect(e.deletes).toEqual({});
  expect(e.stale).toEqual({});
  expect(e.versions).toEqual({ [rowKey(row("2"))]: "0x01" });
  e = keepVersion(setCell(e, row("1"), "name", "x", "a2"), row("1"), "0x09");
  expect(toChanges(e, "dbo", "t", { kind: "Values" }).changes.updates.map((u) => u.expect)).toEqual([
    [{ column: "name", value: "b" }],
    [{ column: "name", value: "a2" }],
  ]);
  expect(toChanges(e, "dbo", "t", { kind: "Rowversion", column: "stamp" }).changes.updates.map((u) => u.expect)).toEqual([
    [{ column: "stamp", value: "0x01" }],
    [{ column: "stamp", value: "0x09" }],
  ]);
});
