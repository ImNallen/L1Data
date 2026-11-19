import type { DatabaseKind, DatabaseRef, RelKind, Relation, RunState } from "../../ipc/bindings";
import { isViewKind, type CatalogView } from "../../state/catalog";
import { sameDb } from "../../state/dbKey";
import { rowResults, runStatements } from "../../state/status";
import { pendingCount, sameTable, type EditTable, type TableEdits } from "../../state/tableEdits";

export type ReadOnlyReason =
  | { kind: "Running" }
  | { kind: "Statements" }
  | { kind: "Plan" }
  | { kind: "Computed"; column: string }
  | { kind: "Renamed"; column: string; source: string }
  | { kind: "Tables"; count: number }
  | { kind: "View"; name: string }
  | { kind: "NotTable"; name: string; relKind: RelKind }
  | { kind: "NoPrimaryKey"; name: string }
  | { kind: "NoKey"; missing: string[] }
  | { kind: "Writes" }
  | { kind: "Calls"; calls: string[] }
  | { kind: "Staged"; name: string; count: number }
  | { kind: "Unknown" };

export type EditTarget =
  | { kind: "Table"; table: EditTable; databaseKind: DatabaseKind; relation: Relation; pkColumns: string[] }
  | { kind: "ReadOnly"; reason: ReadOnlyReason };

const readOnly = (reason: ReadOnlyReason): EditTarget => ({ kind: "ReadOnly", reason });

/**
 * `db` is the tab's current database: Submit runs on the tab's session, so a result read before a retarget stays read-only.
 * `edits` are the tab's staged edits, which bind the tab to the table they were staged against until they are submitted or
 * reverted.
 */
export function editTarget({
  run,
  view,
  db,
  edits,
}: {
  run: RunState;
  view: CatalogView | undefined;
  db: DatabaseRef | null;
  edits: TableEdits | undefined;
}): EditTarget {
  const results = rowResults(run);
  const statements = runStatements(run);
  const only = results.length === 1 && statements.length === 1 && statements[0]!.outputs.length === 1 ? results[0]! : null;
  if (!only) return readOnly({ kind: "Statements" });
  if (only.plan) return readOnly({ kind: "Plan" });
  const { meta } = only;
  // A table page is always one result. A script may still send another.
  if (run.kind === "Running" && meta.allRows.kind !== "Table") return readOnly({ kind: "Running" });
  const computed = meta.columns.find((c) => c.source === null);
  if (computed) return readOnly({ kind: "Computed", column: computed.name });
  const oids = new Set(meta.columns.map((c) => c.source!.tableOid));
  if (oids.size > 1) return readOnly({ kind: "Tables", count: oids.size });
  const sameCatalog = view && view.catalog.databaseKind === meta.databaseKind && sameDb(view.catalog.db, meta.db) && sameDb(meta.db, db);
  const found = sameCatalog && oids.size === 1 ? view.relationsById.get([...oids][0]!) : undefined;
  if (!found) return readOnly({ kind: "Unknown" });
  const { schema, rel } = found;
  const name = `${schema}.${rel.name}`;
  if (isViewKind(rel.kind)) return readOnly({ kind: "View", name });
  if (rel.kind !== "Table") return readOnly({ kind: "NotTable", name, relKind: rel.kind });
  for (const c of meta.columns) {
    const column = rel.columns.find((col) => col.attnum === c.source!.attnum);
    if (!column) return readOnly({ kind: "Unknown" });
    if (column.name !== c.name) return readOnly({ kind: "Renamed", column: c.name, source: column.name });
  }
  const pkColumns = rel.columns.filter((c) => c.pk).map((c) => c.name);
  if (!pkColumns.length) return readOnly({ kind: "NoPrimaryKey", name });
  const missing = pkColumns.filter((pk) => !meta.columns.some((c) => c.name === pk));
  if (missing.length) return readOnly({ kind: "NoKey", missing });
  // A query tab shows what Submit saved by running the statement again, so it must be one that may run twice.
  if (meta.allRows.kind === "Refused") return readOnly({ kind: "Writes" });
  if (meta.allRows.kind === "Confirm") return readOnly({ kind: "Calls", calls: meta.allRows.calls });
  const table: EditTable = { db: meta.db, schema, table: rel.name };
  const count = edits ? pendingCount(edits) : 0;
  if (edits?.table && count > 0 && !sameTable(edits.table, table)) {
    return readOnly({ kind: "Staged", name: `${edits.table.schema}.${edits.table.table}`, count });
  }
  return { kind: "Table", table, databaseKind: meta.databaseKind, relation: rel, pkColumns };
}

const RELATION_KIND: Record<RelKind, string> = {
  Table: "table",
  PartitionedTable: "partitioned table",
  View: "view",
  MaterializedView: "materialized view",
  ForeignTable: "foreign table",
};

const list = (names: string[]) => (names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names.at(-1)}` : (names[0] ?? ""));

export function readOnlyReasonText(reason: ReadOnlyReason): string {
  switch (reason.kind) {
    case "Running":
      return "Read-only until the run finishes.";
    case "Statements":
      return "Read-only: the run returned more than one result. Run one statement to edit its rows.";
    case "Plan":
      return "Read-only: this is a query plan.";
    case "Computed":
      return `Read-only: ${reason.column} does not come straight from a table column.`;
    case "Renamed":
      return `Read-only: ${reason.column} renames ${reason.source}. Select it by its own name to edit.`;
    case "Tables":
      return `Read-only: this result joins ${reason.count} tables.`;
    case "View":
      return `Read-only: ${reason.name} is a view.`;
    case "NotTable":
      return `Read-only: ${reason.name} is a ${RELATION_KIND[reason.relKind]}. Only plain tables edit here.`;
    case "NoPrimaryKey":
      return `Read-only: ${reason.name} has no primary key.`;
    case "NoKey":
      return `Read-only: add the primary key ${reason.missing.length > 1 ? "columns" : "column"} ${list(reason.missing)} to edit.`;
    case "Writes":
      return "Read-only: this statement may write, so L1 Data won't run it again to show what Submit saved.";
    case "Calls":
      return `Read-only: this statement calls ${list(reason.calls)}, which L1 Data won't run again unasked to show what Submit saved.`;
    case "Staged": {
      const it = reason.count === 1 ? "it" : "them";
      return `Read-only: this tab holds ${reason.count === 1 ? "1 staged change" : `${reason.count} staged changes`} to ${reason.name}. Show that table's rows to submit ${it}, or revert ${it}.`;
    }
    case "Unknown":
      return "Read-only: L1 Data can't find this result's table in the loaded catalog.";
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}
