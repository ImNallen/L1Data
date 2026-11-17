import type { DatabaseRef, LinkBasis, MapColumnPair, RelKind, SchemaMap, TypeCategory } from "../../ipc/bindings";

export type Oid = number;
export type MapColumns = "All" | "Keys";
export type TableKey = `${string}.${string}`;

export function tableKey(schema: string, name: string): TableKey {
  return `${schema}.${name}`;
}

export type ColumnRole = "pk" | "fk" | "pkfk" | "inferred" | "plain";

export type ErColumn = {
  attnum: number;
  name: string;
  type: string;
  category: TypeCategory;
  notNull: boolean;
  pk: boolean;
  role: ColumnRole;
  showsInKeysMode: boolean;
};

export type ErTable = {
  oid: Oid;
  key: TableKey;
  schema: string;
  name: string;
  kind: RelKind;
  rowEstimate: number | null;
  partitions: number;
  external: boolean;
  columns: readonly ErColumn[];
};

export type ErLink = {
  id: string;
  basis: LinkBasis;
  name: string | null;
  source: Oid;
  target: Oid;
  pairs: readonly MapColumnPair[];
  enabled: boolean;
  validated: boolean;
  oneToOne: boolean;
};

export type ErModel = {
  db: DatabaseRef;
  version: number;
  schema: string;
  tables: ReadonlyMap<Oid, ErTable>;
  byKey: ReadonlyMap<TableKey, Oid>;
  links: ReadonlyMap<string, ErLink>;
  incident: ReadonlyMap<Oid, readonly string[]>;
  isolated: readonly Oid[];
  keysTruncated: boolean;
  omittedTables: number;
};

export function compareText(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

export function toModel(wire: SchemaMap): ErModel {
  const wireTables = [...wire.tables].sort(
    (a, b) => Number(a.external) - Number(b.external) || compareText(a.schema, b.schema) || compareText(a.name, b.name) || a.oid - b.oid,
  );
  const present = new Set(wireTables.map((t) => t.oid));
  const wireLinks = wire.links
    .filter((l) => present.has(l.source) && present.has(l.target))
    .sort((a, b) => compareText(a.basis, b.basis) || compareText(a.id, b.id));

  const declaredSources = new Set<string>();
  const inferredSources = new Set<string>();
  const ends = new Set<string>();
  const incident = new Map<Oid, string[]>(wireTables.map((t) => [t.oid, []]));
  for (const link of wireLinks) {
    for (const pair of link.columns) {
      (link.basis === "Declared" ? declaredSources : inferredSources).add(`${link.source}.${pair.source}`);
      ends.add(`${link.source}.${pair.source}`).add(`${link.target}.${pair.target}`);
    }
    incident.get(link.source)!.push(link.id);
    if (link.target !== link.source) incident.get(link.target)!.push(link.id);
  }

  const tables = new Map<Oid, ErTable>();
  for (const t of wireTables) {
    const columns = t.columns.map((c): ErColumn => {
      const at = `${t.oid}.${c.name}`;
      const role: ColumnRole = declaredSources.has(at) ? (c.pk ? "pkfk" : "fk") : c.pk ? "pk" : inferredSources.has(at) ? "inferred" : "plain";
      return {
        attnum: c.attnum,
        name: c.name,
        type: c.typeDisplay,
        category: c.category,
        notNull: c.notNull,
        pk: c.pk,
        role,
        showsInKeysMode: c.pk || ends.has(at),
      };
    });
    const { oid, schema, name, kind, rowEstimate, partitions, external } = t;
    tables.set(oid, { oid, key: tableKey(schema, name), schema, name, kind, rowEstimate, partitions, external, columns });
  }

  const links = new Map<string, ErLink>();
  for (const l of wireLinks) {
    const primary = tables.get(l.source)!.columns.filter((c) => c.pk);
    const sources = new Set(l.columns.map((p) => p.source));
    const oneToOne = primary.length > 0 && primary.length === sources.size && primary.every((c) => sources.has(c.name));
    const { id, basis, name, source, target, enabled, validated } = l;
    links.set(id, { id, basis, name, source, target, pairs: l.columns, enabled, validated, oneToOne });
  }

  return {
    db: wire.db,
    version: wire.catalogVersion,
    schema: wire.schema,
    tables,
    byKey: new Map([...tables.values()].map((t) => [t.key, t.oid])),
    links,
    incident,
    isolated: [...tables.values()].filter((t) => incident.get(t.oid)!.length === 0).map((t) => t.oid),
    keysTruncated: wire.keysTruncated,
    omittedTables: wire.omittedTables,
  };
}

export function visibleColumns(table: ErTable, columns: MapColumns): readonly ErColumn[] {
  return columns === "All" ? table.columns : table.columns.filter((c) => c.showsInKeysMode);
}

export type Focus = {
  table: ErTable;
  related: ReadonlySet<Oid>;
  links: readonly ErLink[];
  counts: {
    references: number;
    referencedBy: number;
    possible: number;
  };
};

export function focusOf(model: ErModel, oid: Oid): Focus | null {
  const table = model.tables.get(oid);
  if (!table) return null;
  const links = model.incident.get(oid)!.map((id) => model.links.get(id)!);
  const declared = links.filter((l) => l.basis === "Declared");
  return {
    table,
    related: new Set([oid, ...links.flatMap((l) => [l.source, l.target])]),
    links,
    counts: {
      references: declared.filter((l) => l.source === oid).length,
      referencedBy: declared.filter((l) => l.target === oid).length,
      possible: links.length - declared.length,
    },
  };
}

export function findMatches(model: ErModel, query: string): ReadonlySet<Oid> | null {
  const needle = query.trim().toLowerCase();
  if (!needle) return null;
  const hit = (name: string) => name.toLowerCase().includes(needle);
  return new Set([...model.tables.values()].filter((t) => hit(t.name) || t.columns.some((c) => hit(c.name))).map((t) => t.oid));
}
