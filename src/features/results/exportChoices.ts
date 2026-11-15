import type { ExportFormat, ExportScope, ResultMeta, RowsState } from "../../ipc/bindings";
import { groupCount, pluralRows } from "./format";

/** Keyed by the generated union, so a format added in model.rs fails the typecheck until it has a row here. */
export const EXPORT_FORMATS: Record<ExportFormat, { label: string; extension: string }> = {
  Csv: { label: "CSV", extension: "csv" },
  Json: { label: "JSON", extension: "json" },
  Markdown: { label: "Markdown", extension: "md" },
  SqlInsert: { label: "SQL INSERT", extension: "sql" },
};
export const FORMAT_ORDER = Object.keys(EXPORT_FORMATS) as ExportFormat[];

export type TablePage = { page: number; pageSize: number; exactRowCount: number | null; filtered: boolean };

export type ExportGroup = { scope: ExportScope; label: string };
export type ExportMenuModel = { groups: ExportGroup[]; note: string | null };

/**
 * The engine decides what may run again (`meta.allRows`). This decides what is worth offering:
 * All only when it would write rows the loaded set lacks.
 */
export function exportMenu(meta: ResultMeta, state: RowsState, table: TablePage | null): ExportMenuModel {
  const incomplete = meta.truncated !== null || state !== "Complete";
  const every = { groups: [{ scope: "Loaded" as const, label: pluralRows(meta.rowCount) }], note: null };
  const loaded = `${groupCount(meta.rowCount)} loaded rows`;
  switch (meta.allRows.kind) {
    case "Table": {
      if (table && table.page === 0 && meta.rowCount < table.pageSize && !incomplete) return every;
      const all = table?.exactRowCount != null ? `All ${pluralRows(table.exactRowCount)}` : table?.filtered ? "All filtered rows" : "All rows in the table";
      return {
        groups: [
          { scope: "Loaded", label: `This page · ${pluralRows(meta.rowCount)}` },
          { scope: "All", label: all },
        ],
        note: null,
      };
    }
    case "Statement":
    case "Confirm": {
      if (!incomplete) return every;
      // rowsSeen counts every row the server sent past the cap, but only a finished run sent them all.
      const all = state === "Complete" ? `All ${pluralRows(meta.rowsSeen)}` : "All rows";
      return {
        groups: [
          { scope: "Loaded", label: loaded },
          { scope: "All", label: all },
        ],
        note: null,
      };
    }
    case "Refused":
      return incomplete ? { groups: [{ scope: "Loaded", label: loaded }], note: meta.allRows.reason } : every;
    default: {
      const unreachable: never = meta.allRows;
      return unreachable;
    }
  }
}
