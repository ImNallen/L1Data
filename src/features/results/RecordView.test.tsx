// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import type { ResultMeta, RowPage } from "../../ipc/bindings";
import { dropRowCaches } from "../../state/rowCache";
import { RecordView } from "./RecordView";
import { HeaderIcon } from "./ResultGrid";

const meta: ResultMeta = {
  databaseKind: "Postgres",
  ref: { tabId: "r1", runId: 1, index: 0 },
  db: { connectionId: "c1", database: "shop" },
  columns: [
    { name: "id", type: { display: "bigint", category: "Integer" }, source: null },
    { name: "doc", type: { display: "jsonb", category: "Json" }, source: null },
    { name: "note", type: { display: "text", category: "Text" }, source: null },
  ],
  rowCount: 600,
  rowsSeen: 600,
  truncated: null,
  elapsedMs: 1,
  allRows: { kind: "Statement" },
  repeatedRead: null,
};

const long = (row: number) => `${row}:${"x".repeat(5_000)}`;
const pages: number[] = [];
const cells: number[] = [];

afterEach(() => {
  vi.useRealTimers();
  cleanup();
  clearMocks();
  dropRowCaches("r1");
  pages.length = 0;
  cells.length = 0;
});

function serve({ cutRows = [0], cell = async (row: number) => long(row) }: { cutRows?: number[]; cell?: (row: number) => Promise<string> } = {}) {
  mockIPC((cmd, args) => {
    const a = args as { start: number; len: number; row: number };
    if (cmd === "result_page") {
      pages.push(a.start);
      const rows = Array.from({ length: a.len }, (_, i) => {
        const row = a.start + i;
        return [String(row + 1), `{"n": ${row}}`, cutRows.includes(row) ? long(row).slice(0, 2_000) : null];
      });
      const cut = cutRows.filter((r) => r >= a.start && r < a.start + a.len).map((r) => [r, 2]);
      return { start: a.start, rows, cut } as RowPage;
    }
    if (cmd === "result_cell") {
      cells.push(a.row);
      return cell(a.row);
    }
    throw new Error(`not served: ${cmd}`);
  });
}

const position = () => screen.getByTestId("record-position").textContent;
const valueOf = (col: number) => screen.getByTestId(`record-value-${col}`).textContent;

it("lists every column with its type glyph and its full value", async () => {
  serve();
  render(<RecordView meta={meta} row={0} onClose={() => {}} />);
  await vi.waitFor(() => expect(valueOf(2)).toBe(long(0)));
  expect([position(), valueOf(0), valueOf(1)]).toEqual(["Row 1 of 600", "1", '{"n": 0}']);
  expect(meta.columns.map((_, i) => screen.getByTestId(`record-field-${i}`).querySelector("svg")?.outerHTML)).toEqual(
    meta.columns.map((column) => renderToStaticMarkup(<HeaderIcon column={column} primaryKey={false} />)),
  );
});

it("pages through rows with the arrow keys, loading each page through the row cache", async () => {
  serve();
  const user = userEvent.setup();
  render(<RecordView meta={meta} row={0} onClose={() => {}} />);
  await vi.waitFor(() => expect(valueOf(0)).toBe("1"));
  for (let i = 0; i < 300; i++) await user.keyboard("{ArrowDown}");
  await vi.waitFor(() => expect(valueOf(0)).toBe("301"));
  expect([position(), valueOf(2), pages]).toEqual(["Row 301 of 600", "NULL", [0, 256]]);
  await user.keyboard("{ArrowUp}");
  expect(position()).toBe("Row 300 of 600");
});

it.each(["shiftKey", "altKey", "metaKey", "ctrlKey"])("leaves an arrow with %s to the browser, so a text selection can grow", async (modifier) => {
  serve();
  const user = userEvent.setup();
  render(<RecordView meta={meta} row={0} onClose={() => {}} />);
  await vi.waitFor(() => expect(valueOf(0)).toBe("1"));
  const event = new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true, [modifier]: true });
  screen.getByTestId("record-fields").dispatchEvent(event);
  await user.keyboard("{ArrowDown}");
  expect([position(), event.defaultPrevented]).toEqual(["Row 2 of 600", false]);
});

it("reads full values only for a row it stays on for 120 ms, and ignores a read that ends after it left the row", async () => {
  vi.useFakeTimers();
  let release: (value: string) => void = () => {};
  serve({ cutRows: [0, 1, 2], cell: (row) => (row === 1 ? new Promise((resolve) => (release = resolve)) : Promise.resolve(long(row))) });
  const down = () => fireEvent.keyDown(screen.getByTestId("record-fields"), { key: "ArrowDown" });
  const advance = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));
  render(<RecordView meta={meta} row={0} onClose={() => {}} />);
  await advance(119);
  expect([valueOf(0), cells]).toEqual(["1", []]);

  down();
  await advance(119);
  expect([valueOf(2), cells]).toEqual([`${long(1).slice(0, 2_000)}…`, []]);
  await advance(1);
  expect(cells).toEqual([1]);

  down();
  await advance(120);
  expect([valueOf(2), cells]).toEqual([long(2), [1, 2]]);
  release(long(1));
  await advance(0);
  expect([position(), valueOf(2)]).toEqual(["Row 3 of 600", long(2)]);
});

it("keeps paging with the arrow keys after a click on Next, and keeps focus on a button at the last row", async () => {
  serve({ cutRows: [] });
  const user = userEvent.setup();
  render(<RecordView meta={{ ...meta, rowCount: 3, rowsSeen: 3 }} row={0} onClose={() => {}} />);
  const fields = await screen.findByRole("region", { name: "Fields" });
  expect([document.activeElement === fields, fields.contains(screen.getByTestId("record-position"))]).toEqual([true, false]);

  const next = screen.getByRole("button", { name: "Next row" });
  await user.click(next);
  await user.click(next);
  expect([position(), next.getAttribute("aria-disabled"), document.activeElement === next]).toEqual(["Row 3 of 3", "true", true]);
  await user.keyboard("{ArrowUp}");
  await vi.waitFor(() => expect(valueOf(0)).toBe("2"));
  expect(position()).toBe("Row 2 of 3");
});

it("says so when the result is gone", async () => {
  mockIPC(() => {
    throw { kind: "Gone" };
  });
  render(<RecordView meta={meta} row={0} onClose={() => {}} />);
  expect((await screen.findByText(/no longer available/)).textContent).toBe("This result is no longer available. Run the query again.");
  expect(screen.queryByTestId("record-fields")).toBeNull();
});
