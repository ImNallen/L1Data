import { describe, expect, it } from "vitest";
import type { AllRows, ResultMeta } from "../../ipc/bindings";
import { exportErrorText } from "../../ipc/errors";
import { exportMenu, type TablePage } from "./exportChoices";

const meta = (allRows: AllRows, rowCount: number, rowsSeen = rowCount): ResultMeta => ({
  ref: { tabId: "t", runId: 1, index: 0 },
  db: { connectionId: "c", database: "shop" },
  databaseKind: "Postgres",
  columns: [],
  rowCount,
  rowsSeen,
  truncated: rowsSeen > rowCount ? "RowLimit" : null,
  elapsedMs: 1,
  allRows,
  repeatedRead: null,
});

const page = (over: Partial<TablePage> = {}): TablePage => ({ page: 0, pageSize: 100, exactRowCount: null, filtered: false, ...over });

describe("exportMenu", () => {
  it("offers one group when the loaded rows are every row", () => {
    expect(exportMenu(meta({ kind: "Statement" }, 42), "Complete", null)).toEqual({ groups: [{ scope: "Loaded", label: "42 rows" }], note: null });
    expect(exportMenu(meta({ kind: "Refused", reason: "may write" }, 1), "Complete", null)).toEqual({
      groups: [{ scope: "Loaded", label: "1 row" }],
      note: null,
    });
  });

  it("offers All with the server's full count after a truncated read", () => {
    const model = exportMenu(meta({ kind: "Statement" }, 1000, 1284551), "Complete", null);
    expect(model.groups).toEqual([
      { scope: "Loaded", label: "1,000 loaded rows" },
      { scope: "All", label: "All 1,284,551 rows" },
    ]);
  });

  it("does not claim a count for a read that was cancelled or failed", () => {
    expect(exportMenu(meta({ kind: "Confirm", calls: ["pg_sleep"] }, 10), "Cancelled", null).groups[1]).toEqual({ scope: "All", label: "All rows" });
  });

  it("explains why a truncated write has no All", () => {
    const model = exportMenu(meta({ kind: "Refused", reason: "The statement may write, so it does not run again." }, 100, 500), "Complete", null);
    expect(model).toEqual({ groups: [{ scope: "Loaded", label: "100 loaded rows" }], note: "The statement may write, so it does not run again." });
  });

  it("offers a table page's whole table, naming the exact count when the tab has one", () => {
    expect(exportMenu(meta({ kind: "Table" }, 100), "Complete", page({ page: 2 })).groups).toEqual([
      { scope: "Loaded", label: "This page · 100 rows" },
      { scope: "All", label: "All rows in the table" },
    ]);
    expect(exportMenu(meta({ kind: "Table" }, 100), "Complete", page({ exactRowCount: 25000, filtered: true })).groups[1]).toEqual({
      scope: "All",
      label: "All 25,000 rows",
    });
    expect(exportMenu(meta({ kind: "Table" }, 100), "Complete", page({ filtered: true })).groups[1]).toEqual({ scope: "All", label: "All filtered rows" });
  });

  it("treats a short first page as the whole table", () => {
    expect(exportMenu(meta({ kind: "Table" }, 7), "Complete", page()).groups).toEqual([{ scope: "Loaded", label: "7 rows" }]);
  });
});

describe("exportErrorText", () => {
  it("says the file was not changed when a failure could have touched it", () => {
    expect(exportErrorText({ kind: "Cancelled" }, "/tmp/a.csv")).toBe("Export cancelled. /tmp/a.csv was not changed.");
    expect(exportErrorText({ kind: "Io", message: "No space left on device (os error 28)" }, "/tmp/a.csv")).toBe(
      "Couldn't write /tmp/a.csv: No space left on device (os error 28). /tmp/a.csv was not changed.",
    );
    expect(exportErrorText({ kind: "TransactionAborted" }, "/tmp/a.csv")).toBe("The transaction failed. Roll back, then export.");
  });
});
