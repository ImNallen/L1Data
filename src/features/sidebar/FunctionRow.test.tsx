import { undo } from "@codemirror/commands";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { DatabaseRef, FunctionInfo, SchemaInfo } from "../../ipc/bindings";
import { activeId, shownNotebook, useWorkspace, type Tab } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { dropEditor, editorStateOf, insertAtCursor, moveCursor, seedEditor, textOf } from "../editor/editorStates";
import { filteredSchemaNodes, insertRoutine, schemaNodes } from "./ConnectionTree";
import { cellsState, moveCell } from "../../testing/notebooks";

const postgres = { connectionId: "pg", database: "shop" };
const sqlServer = { connectionId: "mssql", database: "shop" };
const pgFunction: FunctionInfo = {
  oid: 12,
  name: "display name",
  quoted: '"display name"',
  args: "unparsed display arguments",
  returns: "int",
  kind: "Function",
  insertText: "order_count()",
};
const msProcedure: FunctionInfo = { ...pgFunction, kind: "Procedure", insertText: "EXEC [odd]]schema].[proc]]name] NULL, NULL" };
const crossKindCases = [
  { source: sqlServer, destination: postgres, fn: msProcedure, label: "SQL Server" },
  { source: postgres, destination: sqlServer, fn: pgFunction, label: "PostgreSQL" },
];
const configs = {
  pg: connectionConfig("pg", "PostgreSQL"),
  mssql: { ...connectionConfig("mssql", "SQL Server"), databaseKind: "SqlServer" },
} satisfies ReturnType<typeof useWorkspace.getState>["configs"];

beforeEach(() => {
  useWorkspace.setState({ configs });
});

afterEach(() => {
  for (const id of ["active", "other"]) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

/** The routine's row as the tree builds it under a schema's Functions group. */
const routineRow = (db: DatabaseRef, fn: FunctionInfo) => ({ onDoubleClick: () => insertRoutine(db, fn) });

function target(db: DatabaseRef | null) {
  const tab: Tab = { id: "active", title: "Query", console: { db, mode: "AutoCommit" }, table: null, file: null };
  const other: Tab = { ...tab, id: "other", console: { db: db?.connectionId === postgres.connectionId ? sqlServer : postgres, mode: "AutoCommit" } };
  useWorkspace.setState(cellsState([other, tab], "active"));
}

function prepareEdits(db: DatabaseRef | null) {
  target(db);
  seedEditor("active", "SELECT ; -- retain suffix");
  moveCursor("active", 7);
  insertAtCursor("active", "42");
  moveCursor("active", 0);
  seedEditor("other", "SELECT 99");
}

function expectRefused(click: () => void) {
  const workspace = useWorkspace.getState();
  const before = editorStateOf("active");
  click();
  expect(textOf("active")).toBe("SELECT 42; -- retain suffix");
  expect(textOf("other")).toBe("SELECT 99");
  expect(editorStateOf("active").selection.eq(before.selection)).toBe(true);
  expect(activeId(useWorkspace.getState())).toBe(activeId(workspace));
  expect(useWorkspace.getState().tabs).toEqual(workspace.tabs);
  expect(useWorkspace.getState().toast?.text).toBeTruthy();
  let state = editorStateOf("active");
  expect(undo({ state, dispatch: (tr) => (state = tr.state) })).toBe(true);
  expect(state.doc.toString()).toBe("SELECT ; -- retain suffix");
}

it.each([
  { db: sqlServer, kind: "Function", insertText: "[odd]]schema].[scalar]]name]()", prefix: "SELECT " },
  { db: sqlServer, kind: "Function", insertText: "[odd]]schema].[table]]name](NULL, NULL)", prefix: "SELECT * FROM " },
  { db: sqlServer, kind: "Procedure", insertText: msProcedure.insertText, prefix: "" },
  { db: postgres, kind: "Function", insertText: "order_count()", prefix: "SELECT " },
  { db: postgres, kind: "Procedure", insertText: "touch_order()", prefix: "SELECT " },
] satisfies { db: DatabaseRef; kind: FunctionInfo["kind"]; insertText: string; prefix: string }[])(
  "inserts the catalog's $kind template verbatim into a same-kind tab at the active cursor",
  ({ db, kind, insertText, prefix }) => {
    target({ ...db, database: "reporting" });
    seedEditor("active", `${prefix}; -- retain suffix`);
    moveCursor("active", prefix.length);
    seedEditor("other", "SELECT 99");

    routineRow(db, { ...pgFunction, kind, insertText }).onDoubleClick();

    expect(textOf("active")).toBe(`${prefix}${insertText}; -- retain suffix`);
    expect(editorStateOf("active").selection.main.head).toBe(prefix.length + insertText.length);
    expect(textOf("other")).toBe("SELECT 99");
    expect(useWorkspace.getState().tabs.active?.console.db).toEqual({ ...db, database: "reporting" });
    expect(useWorkspace.getState().toast).toBeNull();
  },
);

it.each([
  { source: postgres, config: configs.pg, fn: pgFunction },
  { source: sqlServer, config: configs.mssql, fn: msProcedure },
])("inserts a routine from $source.connectionId into another saved connection of the same kind", ({ source, config, fn }) => {
  const destination = { connectionId: "another", database: "reporting" };
  useWorkspace.getState().upsertConfig({ ...config, id: destination.connectionId });
  prepareEdits(destination);

  routineRow(source, fn).onDoubleClick();

  expect(textOf("active")).toBe(`${fn.insertText}SELECT 42; -- retain suffix`);
  expect(editorStateOf("active").selection.main.head).toBe(fn.insertText.length);
  expect(useWorkspace.getState().tabs.active?.console.db).toEqual(destination);
  expect(useWorkspace.getState().toast).toBeNull();
});

it.each(crossKindCases)("refuses a $label routine in a different-kind active tab", ({ source, destination, fn, label }) => {
  prepareEdits(destination);
  expectRefused(() => routineRow(source, fn).onDoubleClick());
  expect(useWorkspace.getState().toast?.text).toContain(label);
});

it.each([
  { name: "no chosen target", db: null, active: "active" },
  { name: "an unsaved connection", db: { connectionId: "missing", database: "shop" }, active: "active" },
  { name: "a missing active tab", db: postgres, active: "missing" },
  { name: "no active tab", db: postgres, active: null },
])("refuses insertion with $name", ({ db, active }) => {
  prepareEdits(db);
  useWorkspace.setState((s) => {
    const notebook = shownNotebook(s)!;
    return active === null ? { shown: { kind: "start", db: null } } : { notebooks: { ...s.notebooks, [notebook.id]: { ...notebook, active } } };
  });
  expectRefused(() => routineRow(postgres, pgFunction).onDoubleClick());
});

it.each([postgres, sqlServer])("refuses a routine whose source connection was removed for $connectionId", (db) => {
  prepareEdits(db);
  expectRefused(() => routineRow({ connectionId: "removed", database: "shop" }, pgFunction).onDoubleClick());
});

it("checks the destination when double-clicked after its target changes", () => {
  prepareEdits(sqlServer);
  const row = routineRow(sqlServer, msProcedure);
  moveCell("active", postgres);
  expectRefused(row.onDoubleClick);
});

it.each(
  crossKindCases.flatMap((test) => [
    { ...test, tree: "normal" },
    { ...test, tree: "filtered" },
  ]),
)("carries the $label source through the $tree tree and inserts only after the target kind matches", ({ source, destination, fn, tree }) => {
  prepareEdits(destination);
  const expanded = { has: () => true, toggle: () => {} };
  const schema: SchemaInfo = {
    name: "schema",
    quoted: "schema",
    system: false,
    tableCount: 0,
    relations: [],
    functions: [fn],
    sequences: [],
    types: [],
    synonyms: [],
  };
  const nodes =
    tree === "normal"
      ? schemaNodes("unrelated/display/path", 2, source, schema, 1, expanded)
      : filteredSchemaNodes(
          "unrelated/display/path",
          source,
          { name: schema.name, relations: [], functions: [{ fn, ranges: [] }], sequences: [], types: [], synonyms: [] },
          expanded,
        );
  const rows = nodes.filter((n) => n.kind === "row" && n.path === `unrelated/display/path/schema/#functions/${fn.name}`);
  expect(rows).toHaveLength(1);
  for (const row of rows) {
    if (row.kind !== "row" || !row.onDoubleClick) throw new Error("the routine's row has no double-click action");
    expectRefused(row.onDoubleClick);
    moveCell("active", { ...source, database: "reporting" });
    row.onDoubleClick();
    expect(textOf("active")).toBe(`${fn.insertText}SELECT 42; -- retain suffix`);
    expect(editorStateOf("active").selection.main.head).toBe(fn.insertText.length);
    expect(textOf("other")).toBe("SELECT 99");
    expect(useWorkspace.getState().tabs.active?.console.db).toEqual({ ...source, database: "reporting" });
  }
});

it.each(["normal", "filtered"])("keys overloaded routines apart in the %s tree while they share a path", (tree) => {
  const expanded = { has: () => true, toggle: () => {} };
  const overloads = [pgFunction, { ...pgFunction, oid: 13, args: "integer" }];
  const schema: SchemaInfo = {
    name: "schema",
    quoted: "schema",
    system: false,
    tableCount: 0,
    relations: [],
    functions: overloads,
    sequences: [],
    types: [],
    synonyms: [],
  };
  const nodes =
    tree === "normal"
      ? schemaNodes("conn", 2, postgres, schema, 1, expanded)
      : filteredSchemaNodes(
          "conn",
          postgres,
          { name: schema.name, relations: [], functions: overloads.map((fn) => ({ fn, ranges: [] })), sequences: [], types: [], synonyms: [] },
          expanded,
        );
  const routines = nodes.filter((n) => n.kind === "row" && n.path === `conn/schema/#functions/${pgFunction.name}`);
  expect(routines).toHaveLength(2);
  expect(new Set(nodes.map((n) => n.key)).size).toBe(nodes.length);
});
