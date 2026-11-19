import { expect, it } from "vitest";
import type { AllRows, ColumnMeta, DatabaseKind, ResultMeta, RunOutput, RunState } from "../../ipc/bindings";
import { buildCatalogView } from "../../state/catalog";
import { NO_EDITS, setCell } from "../../state/tableEdits";
import { shopCatalog } from "../../testing/shopCatalog";
import { editTarget, readOnlyReasonText } from "./editTarget";

const catalog = shopCatalog("c1", 1);
const view = buildCatalogView(catalog);
const db = catalog.db;
const relation = (name: string) => catalog.schemas[1]!.relations.find((r) => r.name === name)!;

/** A result column read straight from `table.column`, as the server describes it. */
function col(table: string, column: string): ColumnMeta {
  const rel = relation(table);
  const info = rel.columns.find((c) => c.name === column)!;
  return { name: column, type: { display: info.typeDisplay, category: info.category }, source: { tableOid: rel.oid, attnum: info.attnum } };
}
const all = (table: string) => relation(table).columns.map((c) => col(table, c.name));
const computed = (name: string): ColumnMeta => ({ name, type: { display: "bigint", category: "Integer" }, source: null });

function rows(columns: ColumnMeta[], index = 0, allRows: AllRows = { kind: "Statement" }): RunOutput {
  const result: ResultMeta = {
    databaseKind: "Postgres",
    ref: { tabId: "q1", runId: 1, index },
    db,
    columns,
    rowCount: 1,
    rowsSeen: 1,
    truncated: null,
    elapsedMs: 1,
    allRows,
  };
  return { kind: "Rows", result, state: "Complete" };
}

function finished(...statements: RunOutput[][]): RunState {
  return {
    kind: "Finished",
    runId: 1,
    elapsedMs: 1,
    outcome: "Ok",
    statements: statements.map((outputs, index) => ({ index, range: { from: 0, to: 1 }, status: { kind: "Finished" }, outputs })),
  };
}

const target = (run: RunState, edits = NO_EDITS) => editTarget({ run, view, db, edits });
const reason = (run: RunState) => {
  const t = target(run);
  return t.kind === "ReadOnly" ? readOnlyReasonText(t.reason) : t;
};

it("edits a single-table SELECT * as that table", () => {
  expect(target(finished([rows(all("orders"))]))).toMatchObject({
    kind: "Table",
    table: { db, schema: "public", table: "orders" },
    databaseKind: "Postgres",
    pkColumns: ["id"],
  });
});

it("edits a column subset that keeps the primary key", () => {
  expect(target(finished([rows([col("orders", "status"), col("orders", "id")])]))).toMatchObject({ kind: "Table", table: { table: "orders" } });
});

it("asks for the primary key a subset leaves out", () => {
  expect(reason(finished([rows([col("orders", "status"), col("orders", "total")])]))).toBe("Read-only: add the primary key column id to edit.");
});

it("keeps a result with an expression column read-only", () => {
  expect(reason(finished([rows([col("orders", "id"), computed("doubled")])]))).toBe("Read-only: doubled does not come straight from a table column.");
});

it("keeps a join across two tables read-only", () => {
  expect(reason(finished([rows([...all("orders"), col("customers", "email")])]))).toBe("Read-only: this result joins 2 tables.");
});

it("edits a join that selects only one table's columns", () => {
  expect(target(finished([rows(all("customers"))]))).toMatchObject({ kind: "Table", table: { table: "customers" } });
});

it("keeps a view's rows read-only", () => {
  expect(reason(finished([rows(all("paid_orders"))]))).toBe("Read-only: public.paid_orders is a view.");
});

it("keeps a run of two statements read-only, even when each is one table", () => {
  expect(reason(finished([rows(all("orders"))], [rows(all("orders"), 1)]))).toBe(
    "Read-only: the run returned more than one result. Run one statement to edit its rows.",
  );
  expect(target(finished([{ kind: "Command", tag: "SET", affectedRows: null, elapsedMs: 1 }], [rows(all("orders"), 1)]))).toMatchObject({
    kind: "ReadOnly",
    reason: { kind: "Statements" },
  });
});

it("keeps a column read under an alias read-only, since Submit writes by name", () => {
  const renamed = { ...col("orders", "status"), name: "state" };
  expect(target(finished([rows([col("orders", "id"), renamed])]))).toMatchObject({ reason: { kind: "Renamed", column: "state", source: "status" } });
});

it("keeps a statement that may not run again read-only, since a query tab runs it again after Submit", () => {
  const refused = finished([rows(all("orders"), 0, { kind: "Refused", reason: "The statement may write, so it does not run again." })]);
  expect(target(refused)).toMatchObject({ reason: { kind: "Writes" } });
  expect(target(finished([rows(all("orders"), 0, { kind: "Confirm", calls: ["audit"] })]))).toMatchObject({ reason: { kind: "Calls", calls: ["audit"] } });
});

it("keeps a result from another database, or one with no catalog, read-only", () => {
  const run = finished([rows(all("orders"))]);
  expect(editTarget({ run, view, db: { ...db, database: "reporting" }, edits: NO_EDITS })).toMatchObject({ reason: { kind: "Unknown" } });
  expect(editTarget({ run, view: undefined, db, edits: NO_EDITS })).toMatchObject({ reason: { kind: "Unknown" } });
});

it.each<{ catalogKind: DatabaseKind; resultKind: DatabaseKind }>([
  { catalogKind: "SqlServer", resultKind: "Postgres" },
  { catalogKind: "Postgres", resultKind: "SqlServer" },
])("keeps a $resultKind result read-only with a $catalogKind catalog despite valid key provenance", ({ catalogKind, resultKind }) => {
  const output = rows(all("orders"));
  const run = finished([output.kind === "Rows" ? { ...output, result: { ...output.result, databaseKind: resultKind } } : output]);
  const other = buildCatalogView({ ...catalog, databaseKind: catalogKind });
  expect(editTarget({ run, view: other, db, edits: NO_EDITS })).toMatchObject({ reason: { kind: "Unknown" } });
});

it("waits for a script to finish, but not a table page", () => {
  const running = (allRows: AllRows): RunState => ({
    kind: "Running",
    runId: 1,
    startedAtMs: 0,
    statements: [{ index: 0, range: { from: 0, to: 1 }, status: { kind: "Running" }, outputs: [rows(all("orders"), 0, allRows)] }],
  });
  expect(target(running({ kind: "Statement" }))).toMatchObject({ reason: { kind: "Running" } });
  expect(target(running({ kind: "Table" }))).toMatchObject({ kind: "Table", table: { table: "orders" } });
});

it("keeps another table read-only while the tab holds staged edits to the first", () => {
  const orders = { db, schema: "public", table: "orders" };
  const staged = { ...setCell(NO_EDITS, [{ column: "id", value: "1" }], "status", "x", "y"), table: orders };
  expect(target(finished([rows(all("orders"))]), staged)).toMatchObject({ kind: "Table", table: { table: "orders" } });
  const customers = target(finished([rows(all("customers"))]), staged);
  expect(customers.kind === "ReadOnly" && readOnlyReasonText(customers.reason)).toBe(
    "Read-only: this tab holds 1 staged change to public.orders. Show that table's rows to submit it, or revert it.",
  );
});
