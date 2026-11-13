export type DraggedHeights = { editor: number | null; results: number | null };

export type CellSizingInput = {
  viewport: number;
  lines: number;
  lineHeight: number;
  results: null | {
    table: boolean;
    rows: number | undefined;
    pageSize: number;
    rowHeight: number;
    chrome: number;
    pager: boolean;
    chart: boolean;
  };
  focused: boolean;
  dragged: DraggedHeights;
};

export type CellHeights = {
  editor: number;
  results: number | null;
  editorMax: number;
  resultsMin: number;
  resultsMax: number;
};

const HEADING = 40;
const BORDERS = 2;
export const CELL_SCROLL_MARGIN = 8;
const CELL_CHROME = HEADING + BORDERS + 2 * CELL_SCROLL_MARGIN;
export const EDITOR_MIN = 64;
const EDITOR_PADDING = 28;
const EDITOR_AUTO_SHARE = 0.4;
const FILTER_BAR = 38;
const PAGER = 36;
const READ_ONLY_NOTE = 26;
const TABLE_CONTROLS = FILTER_BAR + PAGER + READ_ONLY_NOTE;
const RESULTS_MIN_QUERY = 128;
const RESULTS_MIN_TABLE = RESULTS_MIN_QUERY + TABLE_CONTROLS;
const TABLE_COMPACT = 380;
const QUERY_COMPACT_ROWS = 10;
const QUERY_UNRUN_ROWS = 4;
const CHART_BODY = 260;

export const clamp = (px: number, min: number, max: number) => Math.min(max, Math.max(min, px));

/** Where a growing cell's top sits: at its anchor, raised only as far as keeps its bottom in view, and never above the margin. */
export const pinnedTop = (anchorTop: number, height: number, viewport: number, margin: number) =>
  Math.max(margin, Math.min(anchorTop, viewport - margin - height));

export function cellHeights({ viewport, lines, lineHeight, results, focused, dragged }: CellSizingInput): CellHeights {
  const room = viewport - CELL_CHROME;
  const editorAuto = Math.min(lines * lineHeight + EDITOR_PADDING, Math.round(room * EDITOR_AUTO_SHARE));
  if (!results) {
    const editorMax = Math.max(EDITOR_MIN, room);
    return { editor: clamp(dragged.editor ?? editorAuto, EDITOR_MIN, editorMax), results: null, editorMax, resultsMin: 0, resultsMax: 0 };
  }
  const { table, rows, pageSize, rowHeight, chrome, pager, chart } = results;
  const floor = table ? RESULTS_MIN_TABLE : RESULTS_MIN_QUERY;
  const editorMax = Math.max(EDITOR_MIN, room - floor);
  const editor = clamp(dragged.editor ?? editorAuto, EDITOR_MIN, editorMax);
  const fill = Math.max(floor, room - editor);
  const queryChrome = chrome + (pager ? PAGER : 0);
  // A table loads a page at a time, so before its rows arrive it holds a page's worth.
  const shownRows = Math.min(rows ?? (table ? pageSize : QUERY_UNRUN_ROWS), pageSize);
  const queryContent = chart ? queryChrome + CHART_BODY : queryChrome + shownRows * rowHeight;
  const content = table ? chrome + TABLE_CONTROLS + shownRows * rowHeight : queryContent;
  const compact = table ? TABLE_COMPACT : chart ? queryContent : queryChrome + QUERY_COMPACT_ROWS * rowHeight;
  const auto = focused ? Math.min(content, fill) : Math.min(content, compact, fill);
  return { editor, results: clamp(dragged.results ?? auto, floor, fill), editorMax, resultsMin: floor, resultsMax: fill };
}
