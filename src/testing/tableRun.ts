import type { AllRows, Catalog, ConsoleState, DatabaseRef, RunState, TabId, TxnMode } from "../ipc/bindings";
import { buildCatalogView, useCatalog } from "../state/catalog";
import { dbKey } from "../state/dbKey";
import { useLive } from "../state/live";

/** A finished run of one result that reads every column of `schema.table`, as a table page or a `SELECT *` does. */
export function tableRun(tabId: TabId, catalog: Catalog, schema: string, table: string, allRows: AllRows = { kind: "Table" }, runId = 1): RunState {
  const relation = catalog.schemas.find((s) => s.name === schema)!.relations.find((r) => r.name === table)!;
  const columns = relation.columns.map((c) => ({
    name: c.name,
    type: { display: c.typeDisplay, category: c.category },
    source: { tableOid: relation.oid, attnum: c.attnum },
  }));
  const result = {
    databaseKind: catalog.databaseKind,
    ref: { tabId, runId, index: 0 },
    db: catalog.db,
    columns,
    rowCount: 1,
    rowsSeen: 1,
    truncated: null,
    elapsedMs: 1,
    allRows,
  };
  return {
    kind: "Finished",
    runId,
    elapsedMs: 1,
    outcome: "Ok",
    statements: [{ index: 0, range: { from: 0, to: 1 }, status: { kind: "Finished" }, outputs: [{ kind: "Rows", result, state: "Complete" }] }],
  };
}

/** Loads `catalog` and makes `tabId`'s open console show `schema.table`'s rows. */
export function showTableRows(tabId: TabId, catalog: Catalog, schema: string, table: string, allRows?: AllRows, runId?: number): void {
  useCatalog.setState((s) => ({ views: { ...s.views, [dbKey(catalog.db)]: buildCatalogView(catalog) } }));
  useLive.setState((s) => {
    const console = s.consoles[tabId];
    if (!console) throw new Error(`no console for ${tabId}`);
    return { consoles: { ...s.consoles, [tabId]: { ...console, run: tableRun(tabId, catalog, schema, table, allRows, runId) } } };
  });
}

export function idleConsole(tabId: TabId, db: DatabaseRef, mode: TxnMode = "AutoCommit"): ConsoleState {
  return {
    tabId,
    rev: 1,
    config: { ...db, schema: null, mode },
    session: { kind: "None" },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
}
