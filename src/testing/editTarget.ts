import type { DatabaseKind } from "../ipc/bindings";
import type { EditTarget } from "../features/results/editTarget";
import type { EditTable } from "../state/tableEdits";

/** A result of `table` that edits. Only `gridEditing` reads the relation, so it has no columns here. */
export function tableTarget(table: EditTable, pkColumns: string[], databaseKind: DatabaseKind = "Postgres"): EditTarget {
  const relation = {
    qualifiedName: `${table.schema}.${table.table}`,
    oid: 1,
    name: table.table,
    quoted: table.table,
    kind: "Table" as const,
    rowEstimate: null,
    columns: [],
  };
  return { kind: "Table", table, databaseKind, relation, pkColumns };
}
