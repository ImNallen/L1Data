import { expect, it } from "vitest";
import type { Catalog, ColumnMeta, DatabaseKind, SchemaInfo, TypeCategory, TypeInfo } from "../../ipc/bindings";
import { describeEditor } from "./gridEditorType";

const schema = (name: string, types: TypeInfo[]): SchemaInfo => ({
  name,
  quoted: `"${name}"`,
  system: false,
  tableCount: 0,
  relations: [],
  functions: [],
  sequences: [],
  types,
  synonyms: [],
});
const catalog = (schemas: SchemaInfo[], databaseKind: DatabaseKind = "Postgres"): Catalog => ({
  schemas,
  databaseKind,
  db: { connectionId: "c", database: "db" },
  version: 1,
  defaultSchema: "public",
  extensions: [],
});
const column = (display: string, category: TypeCategory): ColumnMeta => ({ name: "v", source: null, type: { display, category } });
const type = (name: string, oid: number, kind: TypeInfo["kind"]): TypeInfo => ({ name, oid, quoted: `"${name}"`, kind });

it("uses category before display, including SQL Server timestamp/rowversion", () => {
  for (const display of ["timestamp", "rowversion", "date"])
    expect(describeEditor({ column: column(display, "Binary"), dialect: "SqlServer" }).kind).toBe("raw");
  expect(describeEditor({ column: column("numeric(38,18)", "Numeric"), dialect: "Postgres" })).toEqual({ kind: "number", inputMode: "decimal" });
  expect(describeEditor({ column: column("bigint", "Integer"), dialect: "SqlServer" })).toEqual({ kind: "number", inputMode: "numeric" });
  expect(describeEditor({ column: column("date", "Other"), dialect: "Postgres" }).kind).toBe("raw");
  expect(describeEditor({ column: column("timestamp", "Temporal"), dialect: "SqlServer" }).kind).not.toBe("temporal");
  expect(describeEditor({ column: column("interval", "Temporal"), dialect: "Postgres" }).kind).toBe("text");
  expect(describeEditor({ column: column("datetimeoffset(7)", "Temporal"), dialect: "SqlServer" })).toEqual({
    kind: "temporal",
    shape: { kind: "timestamp", zone: "offset" },
  });
  for (const dialect of ["Postgres", "SqlServer"] as const)
    expect(describeEditor({ column: column(dialect === "Postgres" ? "boolean" : "bit", "Bool"), dialect })).toEqual({
      kind: "boolean",
      trueValue: dialect === "Postgres" ? "t" : "1",
      falseValue: dialect === "Postgres" ? "f" : "0",
    });
});

it("resolves quoted qualified enum/domain identities and keeps ordered empty labels", () => {
  const labels = ["Paid", "", "a.b", "CANCELLED"];
  const snapshot = catalog([
    schema("My.Schema", [type('Order"State', 1, { kind: "Enum", labels }), type("domain", 2, { kind: "Domain", base: '"My.Schema"."Order""State"' })]),
  ]);
  for (const display of ['"My.Schema"."Order""State"', '"My.Schema".domain', '"Order""State"'])
    expect(describeEditor({ column: column(display, "Enum"), catalog: snapshot, dialect: "Postgres" })).toEqual({ kind: "enum", labels });
});

it("rejects ambiguous unqualified names, missing metadata and cyclic domains", () => {
  const snapshot = catalog([
    schema("a", [
      type("status", 1, { kind: "Enum", labels: ["a"] }),
      type("one", 3, { kind: "Domain", base: "two" }),
      type("two", 4, { kind: "Domain", base: "one" }),
    ]),
    schema("b", [type("status", 2, { kind: "Enum", labels: ["b"] })]),
  ]);
  for (const display of ["status", "missing", "one", "two", "a.", '"unterminated'])
    expect(describeEditor({ column: column(display, "Enum"), catalog: snapshot, dialect: "Postgres" }).kind).toBe("raw");
  expect(describeEditor({ column: column("status", "Enum"), dialect: "Postgres" }).kind).toBe("raw");
  expect(describeEditor({ column: column("b.status", "Enum"), catalog: snapshot, dialect: "Postgres" })).toEqual({ kind: "enum", labels: ["b"] });
});

it("follows SQL Server alias bases without guessing ambiguous names or overriding categories", () => {
  const snapshot = catalog(
    [
      schema("dbo", [
        type("clock", 1, { kind: "Alias", base: "datetime2(7)", nullable: true }),
        type("cash", 2, { kind: "Alias", base: "decimal(38, 18)", nullable: false }),
      ]),
    ],
    "SqlServer",
  );
  expect(describeEditor({ column: column("[dbo].[clock]", "Other"), catalog: snapshot, dialect: "SqlServer" })).toEqual({
    kind: "temporal",
    shape: { kind: "timestamp", zone: "none" },
  });
  expect(describeEditor({ column: column("dbo.cash", "Other"), catalog: snapshot, dialect: "SqlServer" })).toEqual({ kind: "number", inputMode: "decimal" });
});
