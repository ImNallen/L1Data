import { expect, it } from "vitest";
import { cellHeights, pinnedTop, type CellSizingInput } from "./cellSizing";

const CHROME = 80;
const ROW = 24;

function heights(overrides: Partial<CellSizingInput>) {
  return cellHeights({
    viewport: 858,
    lines: 5,
    lineHeight: 20,
    results: null,
    focused: true,
    dragged: { editor: null, results: null },
    ...overrides,
  });
}
const table = (rows?: number) => ({ table: true, rows, rowHeight: ROW, chrome: CHROME, pager: false, chart: false });
const query = (rows?: number, pager = false, chart = false) => ({ table: false, rows, rowHeight: ROW, chrome: CHROME, pager, chart });
const PAGER = 36;

it("fills the room below the editor with a focused table", () => {
  expect(heights({ results: table(1000) })).toMatchObject({ editor: 128, results: 672 });
});

it("fills the room while a focused table has no row count yet", () => {
  expect(heights({ results: table() }).results).toBe(672);
});

it("sizes a short result to its content", () => {
  expect(heights({ results: query(3) }).results).toBe(152);
  expect(heights({ results: table(3) }).results).toBe(252);
});

it("adds the pager's height to a paged query, focused or compact", () => {
  expect(heights({ results: query(3, true) }).results).toBe(152 + PAGER);
  expect(heights({ results: query(1000, true), focused: false }).results).toBe(320 + PAGER);
});

it("gives an unrun query four rows of space", () => {
  expect(heights({ results: query() }).results).toBe(176);
});

it("keeps unfocused results compact", () => {
  expect(heights({ results: query(1000), focused: false }).results).toBe(320);
  expect(heights({ results: table(1000), focused: false }).results).toBe(380);
});

it("caps an automatic editor at 40% of the room, and a dragged one at what the results leave", () => {
  expect(heights({ lines: 100 }).editor).toBe(320);
  expect(heights({ lines: 100, results: table(1000), dragged: { editor: 2000, results: null } })).toMatchObject({ editor: 572, results: 228 });
});

it("keeps a dragged result inside a shrinking viewport and restores it when the viewport grows", () => {
  const dragged = { editor: null, results: 600 };
  expect(heights({ results: table(1000), dragged }).results).toBe(600);
  const short = heights({ viewport: 400, results: table(1000), dragged });
  expect(short).toMatchObject({ editor: 114, results: 228 });
  expect(short.editor + (short.results ?? 0) + 58).toBeLessThanOrEqual(400);
  expect(heights({ results: table(1000), dragged }).results).toBe(600);
});

it("gives automatic results less room when the editor is dragged taller", () => {
  expect(heights({ results: table(1000), dragged: { editor: 400, results: null } })).toMatchObject({ editor: 400, results: 400, resultsMax: 400 });
});

it("never drops results under their floor", () => {
  expect(heights({ viewport: 300, results: table(1000) }).results).toBe(228);
  expect(heights({ viewport: 200, results: query(1000) }).results).toBe(128);
  expect(heights({ results: table(1000), dragged: { editor: null, results: 50 } }).results).toBe(228);
  expect(heights({ results: query(1000), dragged: { editor: null, results: 50 } }).results).toBe(128);
});

it("leaves the editor more room to grow when the cell shows no results", () => {
  expect(heights({}).editorMax).toBe(800);
  expect(heights({ results: query(10) }).editorMax).toBe(672);
  expect(heights({ results: table(10) }).editorMax).toBe(572);
  expect(heights({}).results).toBeNull();
});

it("holds the minimum sizes before the first measure", () => {
  expect(heights({ viewport: 0, results: table(1000) })).toMatchObject({ editor: 64, results: 228 });
});

it("gives a charted result the chart's height whatever its row count", () => {
  expect(heights({ results: query(6, false, true) }).results).toBe(340);
  expect(heights({ results: query(1000, false, true), focused: false }).results).toBe(340);
  expect(heights({ results: query(6) }).results).toBe(224);
});

it("keeps a growing cell at its anchor while its bottom stays in view", () => {
  expect(pinnedTop(300, 400, 900, 8)).toBe(300);
});

it("raises a growing cell only as far as its bottom needs", () => {
  expect(pinnedTop(500, 487, 945, 8)).toBe(450);
  expect(pinnedTop(500, 929, 945, 8)).toBe(8);
});

it("pins a cell taller than the viewport, or one above it, at the margin", () => {
  expect(pinnedTop(300, 2000, 900, 8)).toBe(8);
  expect(pinnedTop(-200, 400, 900, 8)).toBe(8);
});
