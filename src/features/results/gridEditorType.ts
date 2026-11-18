import type { Catalog, ColumnInfo, ColumnMeta, DatabaseKind, TypeCategory } from "../../ipc/bindings";

export type TemporalShape = { kind: "date" } | { kind: "time" | "timestamp"; zone: "none" | "offset" };
export type EditorDescriptor =
  | { kind: "text" | "raw"; hint: string }
  | { kind: "number"; inputMode: "numeric" | "decimal" }
  | { kind: "boolean"; trueValue: "t" | "1"; falseValue: "f" | "0" }
  | { kind: "enum"; labels: readonly string[] }
  | { kind: "temporal"; shape: TemporalShape }
  | { kind: "json" };

function identifiers(text: string, dialect: DatabaseKind): string[] | null {
  const parts: string[] = [];
  let rest = text.trim();
  while (rest) {
    const match = /^(?:"((?:[^"]|"")*)"|\[((?:[^\]]|\]\])*)\]|([\p{L}_][\p{L}\p{N}_$]*))/u.exec(rest);
    if (!match) return null;
    parts.push(
      match[1] !== undefined
        ? match[1].replaceAll('""', '"')
        : match[2] !== undefined
          ? match[2].replaceAll("]]", "]")
          : dialect === "Postgres"
            ? match[3]!.toLowerCase()
            : match[3]!,
    );
    rest = rest.slice(match[0].length).trim();
    if (!rest) break;
    if (!rest.startsWith(".")) return null;
    rest = rest.slice(1).trim();
    if (!rest) return null;
  }
  return parts.length > 0 && parts.length <= 2 ? parts : null;
}

type Builtin = { category: "Temporal"; shape: TemporalShape } | { category: Exclude<TypeCategory, "Temporal"> };

function builtin(display: string, dialect: DatabaseKind): Builtin | null {
  const name = display
    .toLowerCase()
    .replace(/\(\s*\d+(?:\s*,\s*\d+)?\s*\)/g, "")
    .replace(/^pg_catalog\./, "")
    .replace(/\s+/g, " ")
    .trim();
  if (name === "date") return { category: "Temporal", shape: { kind: "date" } };
  if (dialect === "SqlServer" && (name === "timestamp" || name === "rowversion")) return { category: "Binary" };
  if (["time", "time without time zone", "time with time zone", "timetz"].includes(name))
    return { category: "Temporal", shape: { kind: "time", zone: name === "timetz" || name === "time with time zone" ? "offset" : "none" } };
  const timestamps =
    dialect === "Postgres"
      ? ["timestamp", "timestamp without time zone", "timestamp with time zone", "timestamptz"]
      : ["datetime", "smalldatetime", "datetime2", "datetimeoffset"];
  if (timestamps.includes(name))
    return {
      category: "Temporal",
      shape: { kind: "timestamp", zone: ["timestamptz", "timestamp with time zone", "datetimeoffset"].includes(name) ? "offset" : "none" },
    };
  if (["int", "int2", "int4", "int8", "integer", "smallint", "bigint", "tinyint", "oid"].includes(name)) return { category: "Integer" };
  if (["numeric", "decimal", "money", "smallmoney", "real", "float", "double precision"].includes(name)) return { category: "Numeric" };
  if (["bool", "boolean", "bit"].includes(name)) return { category: "Bool" };
  if (["json", "jsonb"].includes(name)) return { category: "Json" };
  if (["text", "varchar", "nvarchar", "char", "nchar", "character varying", "character"].includes(name)) return { category: "Text" };
  return null;
}

export function describeEditor({
  column,
  sourceColumn,
  catalog,
  dialect,
}: {
  column: ColumnMeta;
  sourceColumn?: ColumnInfo;
  catalog?: Catalog;
  dialect: DatabaseKind;
}): EditorDescriptor {
  const raw: EditorDescriptor = { kind: "raw", hint: `Exact ${column.type.display} database syntax` };
  switch (column.type.category) {
    case "Integer":
      return { kind: "number", inputMode: "numeric" };
    case "Numeric":
      return { kind: "number", inputMode: "decimal" };
    case "Bool":
      return { kind: "boolean", trueValue: dialect === "Postgres" ? "t" : "1", falseValue: dialect === "Postgres" ? "f" : "0" };
    case "Json":
      return { kind: "json" };
    case "Text":
      return { kind: "text", hint: "Text, preserving spaces and line breaks" };
    case "Uuid":
      return { kind: "text", hint: "UUID, for example 550e8400-e29b-41d4-a716-446655440000" };
    case "Network":
      return { kind: "text", hint: "Network address, optionally with a /prefix" };
    case "Temporal":
    case "Enum":
    case "Other":
      break;
    default:
      return raw;
  }
  const displayName = sourceColumn?.typeDisplay ?? column.type.display;
  if (
    column.type.category === "Temporal" &&
    /^interval(?:\(\d+\))?(?: (?:year|month|day|hour|minute|second)(?: to (?:month|hour|minute|second))?)?$/i.test(displayName)
  ) {
    return { kind: "text", hint: "Interval, for example 2 days 03:00:00" };
  }
  let display = displayName;
  let resolved = builtin(display, dialect);
  let labels: readonly string[] | undefined;
  const visited = new Set<number>();
  while (!resolved) {
    const names = identifiers(display, dialect);
    if (!names || !catalog || catalog.databaseKind !== dialect) break;
    const matches = catalog.schemas.flatMap((schema) =>
      schema.types.filter((type) => type.name === names.at(-1) && (names.length === 1 || schema.name === names[0])),
    );
    if (matches.length !== 1) return raw;
    const type = matches[0]!;
    if (visited.has(type.oid)) return raw;
    visited.add(type.oid);
    if (type.kind.kind === "Enum") {
      labels = type.kind.labels;
      break;
    }
    if (type.kind.kind !== "Domain" && type.kind.kind !== "Alias") break;
    display = type.kind.base;
    resolved = builtin(display, dialect);
  }
  const category = column.type.category;
  if (category === "Enum" || (category === "Other" && labels)) return labels ? { kind: "enum", labels } : raw;
  if (category === "Temporal") {
    if (resolved?.category === "Temporal") return { kind: "temporal", shape: resolved.shape };
    return raw;
  }
  if (visited.size && resolved) return describeEditor({ column: { ...column, type: { display, category: resolved.category } }, dialect });
  return raw;
}
