import {
  BinaryIcon,
  BoxIcon,
  BracesIcon,
  BracketsIcon,
  CalendarIcon,
  CaseSensitiveIcon,
  ChevronDownIcon,
  ChevronUpIcon,
  CircleDotIcon,
  FingerprintPatternIcon,
  FunnelIcon,
  HashIcon,
  KeyRoundIcon,
  ListIcon,
  MoveDownIcon,
  MoveHorizontalIcon,
  MoveUpIcon,
  NetworkIcon,
  ToggleLeftIcon,
  XIcon,
  type LucideIcon,
} from "lucide-react";
import { useVirtualizer, type VirtualItem } from "@tanstack/react-virtual";
import {
  memo,
  useEffect,
  useId,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type Ref,
} from "react";
import { commands, type ColumnMeta, type ColSpan, type Density, type FontSetting, type ResultMeta, type RowSpan, type TypeCategory } from "../../ipc/bindings";
import { DENSITY_FACTOR, gridRowHeight } from "../../shell/appearance";
import { isMac } from "../../shell/hostedWindow";
import { rowCacheFor, type Row, type RowCache } from "../../state/rowCache";
import { findRelation, isPrimaryKey, useCatalog, viewFor, type CatalogView } from "../../state/catalog";
import { sameDb, type DbKey } from "../../state/dbKey";
import {
  NO_EDITS,
  markDeleted,
  removeNewRows,
  resetNewCell,
  rowKey,
  setCell,
  setNewCell,
  useTableEdits,
  type CellValue,
  type RowId,
  type TableEdits,
} from "../../state/tableEdits";
import { useWorkspace } from "../../state/workspace";
import { fontStack } from "../../themes/fonts";
import { ContextMenuArea, ContextMenuItem, ContextMenuSeparator, IconButton, cx } from "../../ui/primitives";
import { openTableTab } from "../editor/runActions";
import { useKeyLinks, type KeyLink } from "./foreignKeys";
import { cellAt, DEFAULT, EMPTY_MARK, GridEditor, GridRow, NULL_MARK, rowNumberAt, type Cell } from "./GridRows";
import { groupDigits, isNumeric, toTsv } from "./format";
import { formatSelectionTotals, type SelectionSlot } from "./selectionTotals";
import { useResultFind } from "./useResultFind";
import { referencedRows, type SortKey } from "./tableQuery";

const HEADER_H = 28;
const ROWNUM_W = 36;
const MIN_COL = 56;
const RESIZE_MIN = 40;
const CELL_PAD_X = 8;
const CELL_BORDER = 1;
const MAX_COL = 320;
const MONO_EM = 0.6;
const SANS_EM = 0.576;
const PREFETCH = 64;
const COPY_ROW_LIMIT = 50_000;
const RESIZE_STEP = 16;

const MAC = typeof navigator !== "undefined" && isMac(navigator.userAgent);
const MOD_KEY = MAC ? "⌘" : "Ctrl+";
const ALT_KEY = MAC ? "⌥" : "Alt+";

type Selection = { anchor: Cell; focus: Cell } | null;

const HEADER_ICON = 12;
const HEADER_GAP = 6;
const SORT_BADGE_W = 20;
const FILTER_BUTTON_W = 20;
const QUERY_CONTROLS = SORT_BADGE_W + FILTER_BUTTON_W + 2 * HEADER_GAP;

export type GridEditing = { pkColumns: string[]; writableColumns: ReadonlySet<string> };
export type GridActions = { addRow(): void; deleteSelectedRows(): void };

export function gridEditing(
  view: CatalogView | undefined,
  schema: string,
  table: string,
  meta: Pick<ResultMeta, "databaseKind" | "db" | "columns">,
): GridEditing | null {
  if (view?.catalog.databaseKind !== "Postgres" || meta.databaseKind !== "Postgres") return null;
  const relation = findRelation(view, schema, table);
  if (!relation || relation.kind !== "Table" || !sameDb(view?.catalog.db, meta.db)) return null;
  const pkColumns = relation.columns.filter((c) => c.pk).map((c) => c.name);
  if (!pkColumns.length || !pkColumns.every((name) => meta.columns.some((c) => c.name === name))) return null;
  if (
    !meta.columns.every(
      (c) => c.source?.tableOid === relation.oid && relation.columns.some((column) => column.attnum === c.source?.attnum && column.name === c.name),
    )
  )
    return null;
  return { pkColumns, writableColumns: new Set(relation.columns.filter((c) => c.writable).map((c) => c.name)) };
}

export function gridKeyColumns(editing: GridEditing | null, edits: TableEdits): string[] {
  const original = Object.values(edits.updates)[0]?.key ?? Object.values(edits.deletes)[0];
  return original ? original.map((cell) => cell.column) : (editing?.pkColumns ?? []);
}

export type GridQuery = {
  sort: SortKey[];
  filtered: Set<string>;
  onSort(column: string, additive: boolean): void;
  onFilterColumn(column: string): void;
  onFilterValue(column: string, value: string | null, negate: boolean): void;
};

const TYPE_ICONS: Record<TypeCategory, LucideIcon> = {
  Integer: HashIcon,
  Numeric: HashIcon,
  Text: CaseSensitiveIcon,
  Bool: ToggleLeftIcon,
  Temporal: CalendarIcon,
  Json: BracesIcon,
  Uuid: FingerprintPatternIcon,
  Binary: BinaryIcon,
  Network: NetworkIcon,
  Array: BracketsIcon,
  Enum: ListIcon,
  Composite: BoxIcon,
  Range: MoveHorizontalIcon,
  Other: CircleDotIcon,
};

export function HeaderIcon({ column, primaryKey }: { column: ColumnMeta; primaryKey: boolean }) {
  if (primaryKey) return <KeyRoundIcon fill="currentColor" size={HEADER_ICON} className="shrink-0 text-amber" />;
  const TypeIcon = TYPE_ICONS[column.type.category];
  return <TypeIcon size={HEADER_ICON} className="shrink-0 text-faint" />;
}

let measurer: CanvasRenderingContext2D | null | undefined;

function textWidth(text: string, size: number, family: string, weight: number | string, em: number): number {
  if (measurer === undefined) measurer = typeof document === "undefined" ? null : document.createElement("canvas").getContext("2d");
  if (!measurer) return text.length * size * em;
  measurer.font = `${weight} ${size}px ${family}`;
  return measurer.measureText(text).width;
}

type GridLook = { codeFont: FontSetting; interfaceFont: FontSetting; density: Density };

const codeCharWidth = ({ codeFont }: GridLook) => textWidth("0", codeFont.size, fontStack("code", codeFont.family), 400, MONO_EM);

// The header label is text-ui-12.5, medium weight.
const headerTextWidth = (name: string, { interfaceFont }: GridLook) =>
  textWidth(name, (12.5 * interfaceFont.size) / 13, fontStack("interface", interfaceFont.family), 500, SANS_EM);

// A NULL or empty mark is italic text-ui-11 with px-1 padding and a 1 px border, so it grows with the interface size, not the code font.
const markWidth = (text: string, { interfaceFont, density }: GridLook) =>
  textWidth(text, (11 * interfaceFont.size) / 13, fontStack("interface", interfaceFont.family), "italic 400", SANS_EM) + 8 * DENSITY_FACTOR[density] + 2;

function naturalWidths(columns: ColumnMeta[], cache: RowCache, sampleRows: number, headerExtra: number, look: GridLook): number[] {
  const charW = codeCharWidth(look);
  // Padding, gaps, and header controls are spacing utilities, so they follow the density's scale.
  const factor = DENSITY_FACTOR[look.density];
  const chrome = 2 * CELL_PAD_X * factor + CELL_BORDER;
  const nullW = markWidth(NULL_MARK, look);
  const emptyW = markWidth(EMPTY_MARK, look);
  return columns.map((c, i) => {
    let longest = 0;
    let mark = 0;
    for (let r = 0; r < sampleRows; r++) {
      const v = cache.get(r)?.[i];
      if (v == null) mark = Math.max(mark, nullW);
      else if (v === "") mark = Math.max(mark, emptyW);
      else longest = Math.max(longest, isNumeric(c.type.category) ? groupDigits(v).length : Math.min(v.length, 60));
    }
    const header = headerTextWidth(c.name, look) + HEADER_ICON + (HEADER_GAP + headerExtra) * factor;
    const content = Math.max(header, longest * charW, mark);
    return Math.min(MAX_COL, Math.max(MIN_COL, Math.ceil(content + chrome)));
  });
}

const STEPS: Record<string, [number, number]> = {
  ArrowUp: [-1, 0],
  ArrowDown: [1, 0],
  ArrowLeft: [0, -1],
  ArrowRight: [0, 1],
  PageUp: [-20, 0],
  PageDown: [20, 0],
};

export function nextFocus(key: string, mod: boolean, from: Cell, rowCount: number, colCount: number): Cell | null {
  const lastRow = rowCount - 1;
  const lastCol = colCount - 1;
  if (key === "Home") return { row: mod ? 0 : from.row, col: 0 };
  if (key === "End") return { row: mod ? lastRow : from.row, col: lastCol };
  if (mod && key.startsWith("Arrow")) {
    const edges: Record<string, Cell> = {
      ArrowUp: { row: 0, col: from.col },
      ArrowDown: { row: lastRow, col: from.col },
      ArrowLeft: { row: from.row, col: 0 },
      ArrowRight: { row: from.row, col: lastCol },
    };
    return edges[key] ?? null;
  }
  const step = STEPS[key];
  if (!step) return null;
  const [dr, dc] = step;
  return {
    row: Math.max(0, Math.min(lastRow, from.row + dr)),
    col: Math.max(0, Math.min(lastCol, from.col + dc)),
  };
}

export function selectRows(anchorRow: number, focusRow: number, colCount: number): NonNullable<Selection> {
  const firstCol = 0;
  const lastCol = colCount - 1;
  return { anchor: { row: anchorRow, col: lastCol }, focus: { row: focusRow, col: firstCol } };
}

function fullRowRange(sel: Selection, colCount: number): [number, number] | null {
  if (!sel || Math.min(sel.anchor.col, sel.focus.col) !== 0 || Math.max(sel.anchor.col, sel.focus.col) !== colCount - 1) return null;
  return [Math.min(sel.anchor.row, sel.focus.row), Math.max(sel.anchor.row, sel.focus.row)];
}

function selectionBounds(sel: NonNullable<Selection>) {
  return {
    r0: Math.min(sel.anchor.row, sel.focus.row),
    r1: Math.max(sel.anchor.row, sel.focus.row),
    c0: Math.min(sel.anchor.col, sel.focus.col),
    c1: Math.max(sel.anchor.col, sel.focus.col),
  };
}

function visualCellCount(sel: NonNullable<Selection>): number {
  const { r0, r1, c0, c1 } = selectionBounds(sel);
  return (r1 - r0 + 1) * (c1 - c0 + 1);
}

function storedSpan(sel: NonNullable<Selection>, rowCount: number): { rows: RowSpan; cols: ColSpan } | null {
  const { r0, r1, c0, c1 } = selectionBounds(sel);
  if (rowCount === 0 || r0 >= rowCount) return null;
  return { rows: { start: r0, end: Math.min(r1, rowCount - 1) }, cols: { start: c0, end: c1 } };
}

function selectionTouchesPending(
  sel: NonNullable<Selection>,
  rowCount: number,
  columns: ColumnMeta[],
  edits: TableEdits,
  cache: RowCache,
  keyOf: (row: number) => string | null,
): boolean {
  const { r0, r1, c0, c1 } = selectionBounds(sel);
  const names = columns.slice(c0, c1 + 1).map((column) => column.name);
  const unseen = Object.keys(edits.updates).length > 0 || Object.keys(edits.deletes).length > 0;
  for (let row = r0; row <= r1; row++) {
    if (row >= rowCount) return true;
    if (!cache.get(row)) {
      if (unseen) return true;
      continue;
    }
    const key = keyOf(row);
    if (!key) {
      if (unseen) return true;
      continue;
    }
    if (key in edits.deletes) return true;
    const values = edits.updates[key]?.values;
    if (values && names.some((name) => name in values)) return true;
  }
  return false;
}

function inSelection(sel: Selection, row: number, col: number): boolean {
  if (!sel) return false;
  const r0 = Math.min(sel.anchor.row, sel.focus.row);
  const r1 = Math.max(sel.anchor.row, sel.focus.row);
  const c0 = Math.min(sel.anchor.col, sel.focus.col);
  const c1 = Math.max(sel.anchor.col, sel.focus.col);
  return row >= r0 && row <= r1 && col >= c0 && col <= c1;
}

/** `pending(row, col)` is the value a pending edit or new row shows there, or undefined for the stored value. */
async function copySelection(
  cache: RowCache,
  sel: NonNullable<Selection>,
  rowCount: number,
  total: number,
  pending: (row: number, col: number) => CellValue | undefined,
) {
  const notify = useWorkspace.getState().notify;
  const r0 = Math.min(sel.anchor.row, sel.focus.row);
  const r1 = Math.min(Math.max(sel.anchor.row, sel.focus.row), total - 1);
  const c0 = Math.min(sel.anchor.col, sel.focus.col);
  const c1 = Math.max(sel.anchor.col, sel.focus.col);
  if (r1 - r0 + 1 > COPY_ROW_LIMIT) {
    notify(`Copy is limited to ${groupDigits(String(COPY_ROW_LIMIT))} rows. Use Export for the full result.`);
    return;
  }
  if (r0 < rowCount) await cache.ensure({ start: r0, end: Math.min(r1, rowCount - 1) + 1 }, rowCount);
  const rows: (string | null)[][] = [];
  for (let r = r0; r <= r1; r++) {
    const row = cache.get(r);
    const out: (string | null)[] = [];
    for (let c = c0; c <= c1; c++) {
      const shown = pending(r, c);
      if (shown !== undefined) out.push(shown);
      else if (r >= rowCount) out.push(null);
      else out.push(cache.isCut(r, c) ? ((await cache.loadFull(r, c)) ?? null) : (row?.[c] ?? null));
    }
    rows.push(out);
  }
  await navigator.clipboard.writeText(toTsv(rows));
  const cells = (r1 - r0 + 1) * (c1 - c0 + 1);
  if (cells > 1) notify(`Copied ${groupDigits(String(cells))} cells.`);
}

/** Which columns are primary keys, per the catalog of the database the rows came from. */
export function primaryKeyColumns(views: Record<DbKey, CatalogView>, meta: Pick<ResultMeta, "db" | "columns">): boolean[] {
  return primaryKeysIn(viewFor(views, meta.db), meta.columns);
}

const primaryKeysIn = (view: CatalogView | undefined, columns: ColumnMeta[]) => columns.map((c) => isPrimaryKey(view, c.source));

type HeaderProps = {
  columns: ColumnMeta[];
  virtualCols: VirtualItem[];
  pk: boolean[];
  focusCol: number;
  query: GridQuery | null;
  editing: GridEditing | null;
  height: number;
  rowNumW: number;
  onResizeStart(e: ReactMouseEvent, col: number): void;
  onResetWidth(col: number): void;
};

const GridHeader = memo(function GridHeader({ columns, virtualCols, pk, focusCol, query, editing, height, rowNumW, onResizeStart, onResetWidth }: HeaderProps) {
  return (
    <div role="row" aria-rowindex={1} className="sticky top-0 z-20 border-b border-grid bg-bg" style={{ height }}>
      <div
        role="columnheader"
        aria-colindex={1}
        aria-label="Row number"
        className="sticky left-0 z-10 flex h-full items-center justify-center border-r border-grid bg-bg font-mono text-code-sm text-dim"
        style={{ width: rowNumW }}
      >
        #
      </div>
      {virtualCols.map((vc) => {
        const c = columns[vc.index]!;
        const sortAt = query ? query.sort.findIndex((k) => k.column === c.name) : -1;
        const sortKey = query?.sort[sortAt];
        const filtered = query?.filtered.has(c.name) ?? false;
        return (
          <div
            key={vc.key}
            data-testid={`grid-header-${vc.index}`}
            role="columnheader"
            aria-colindex={vc.index + 2}
            aria-sort={sortKey ? (sortKey.desc ? "descending" : "ascending") : undefined}
            title={`${c.name} ${c.type.display}${editing && !editing.writableColumns.has(c.name) ? "\nRead-only column" : ""}${query ? `\nClick to sort, Shift-click to add to the sort. ${ALT_KEY}S sorts by the selected cell's column.` : ""}`}
            onClick={query ? (e) => query.onSort(c.name, e.shiftKey) : undefined}
            className={cx(
              "group/header absolute top-0 flex h-full items-center gap-1.5 border-r border-grid px-2",
              focusCol === vc.index ? "bg-hover text-fg" : "text-muted",
              query && "cursor-pointer hover:bg-hover hover:text-fg",
            )}
            style={{ left: vc.start, width: vc.size }}
          >
            <HeaderIcon column={c} primaryKey={pk[vc.index]!} />
            <span className="min-w-0 truncate text-ui-12.5 font-medium">{c.name}</span>
            {sortKey && (
              <span data-testid={`grid-sort-${vc.index}`} className="flex shrink-0 items-center text-keyword">
                {sortKey.desc ? <MoveDownIcon size={13} /> : <MoveUpIcon size={13} />}
                {query!.sort.length > 1 && <span className="font-mono text-ui-10">{sortAt + 1}</span>}
              </span>
            )}
            {query && (
              <button
                type="button"
                data-testid={`grid-filter-${vc.index}`}
                aria-label={`Filter ${c.name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  query.onFilterColumn(c.name);
                }}
                className={cx(
                  "ml-auto grid size-5 shrink-0 place-items-center rounded hover:bg-control",
                  filtered ? "text-keyword" : "text-dim opacity-0 group-hover/header:opacity-100 focus-visible:opacity-100",
                )}
              >
                <FunnelIcon size={12} />
              </button>
            )}
            <div
              data-testid={`grid-resize-${vc.index}`}
              title={`Drag to resize, double-click to reset. ${ALT_KEY}← and ${ALT_KEY}→ resize the selected cell's column.`}
              onClick={(e) => e.stopPropagation()}
              onMouseDown={(e) => onResizeStart(e, vc.index)}
              onDoubleClick={() => onResetWidth(vc.index)}
              className="absolute top-0 -right-[3px] z-10 h-full w-[5px] cursor-col-resize hover:bg-focus"
            />
          </div>
        );
      })}
    </div>
  );
});

type GridProps = {
  meta: ResultMeta;
  done: boolean;
  query: GridQuery | null;
  editing: GridEditing | null;
  actionsRef?: Ref<GridActions | null>;
  onSelectionTotals?: (slot: SelectionSlot) => void;
};

/**
 * The results pane builds `query` and `onSelectionTotals` afresh on each of its renders. This hands the grid props that stay
 * the same while the sort and the filtered columns do, and routes each callback to the latest one.
 */
export function ResultGrid(props: GridProps) {
  const latest = useRef(props);
  latest.current = props;
  const { query } = props;
  const sortKey = query ? JSON.stringify(query.sort) : null;
  const filteredKey = query ? [...query.filtered].join("\u0000") : null;
  const stableQuery = useMemo<GridQuery | null>(
    () =>
      query && {
        sort: query.sort,
        filtered: query.filtered,
        onSort: (column, additive) => latest.current.query?.onSort(column, additive),
        onFilterColumn: (column) => latest.current.query?.onFilterColumn(column),
        onFilterValue: (column, value, negate) => latest.current.query?.onFilterValue(column, value, negate),
      },
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the keys stand for the sort and filters, which arrive in new objects on every render
    [sortKey, filteredKey],
  );
  const reportsTotals = props.onSelectionTotals !== undefined;
  const onSelectionTotals = useMemo(() => (reportsTotals ? (slot: SelectionSlot) => latest.current.onSelectionTotals?.(slot) : undefined), [reportsTotals]);
  return (
    <GridView
      meta={props.meta}
      done={props.done}
      query={stableQuery}
      editing={props.editing}
      actionsRef={props.actionsRef}
      onSelectionTotals={onSelectionTotals}
    />
  );
}

const GridView = memo(function GridView({ meta, done, query, editing, actionsRef, onSelectionTotals }: GridProps) {
  const cache = rowCacheFor(meta.ref);
  const cacheVersion = useSyncExternalStore(cache.subscribe, cache.getVersion);
  const scroller = useRef<HTMLDivElement>(null);
  const [selection, setSelection] = useState<Selection>(null);
  const dragging = useRef<"cells" | "rows" | null>(null);
  const { rowCount, columns } = meta;
  const catalogView = useCatalog((s) => viewFor(s.views, meta.db));
  const pk = useMemo(() => primaryKeysIn(catalogView, columns), [catalogView, columns]);
  const tabId = meta.ref.tabId;
  const edits = useTableEdits((s) => s.byTab[tabId] ?? NO_EDITS);
  const pkColumns = useMemo(() => gridKeyColumns(editing, edits), [editing, edits]);
  const pkIdx = useMemo(() => pkColumns.map((n) => columns.findIndex((c) => c.name === n)), [pkColumns, columns]);
  const hasKey = pkIdx.length > 0 && pkIdx.every((i) => i >= 0);
  const editable = editing !== null && hasKey;
  const edit = (f: (e: TableEdits) => TableEdits) => useTableEdits.getState().edit(tabId, f);
  const total = rowCount + edits.inserts.length;
  const newRowAt = (row: number) => (row >= rowCount ? edits.inserts[row - rowCount] : undefined);
  const rowIdOf = (row: number): RowId | null => {
    const values = cache.get(row);
    if (!values || row >= rowCount || !hasKey) return null;
    return pkIdx.map((index, i) => ({ column: pkColumns[i]!, value: values[index] ?? null }));
  };
  // Each loaded row's key is built once, not on every render of the rows on screen.
  const keySig = `${pkColumns.join("\u0000")}|${pkIdx.join(",")}`;
  const keyCache = useRef({ sig: keySig, keys: new WeakMap<Row, string>() });
  if (keyCache.current.sig !== keySig) keyCache.current = { sig: keySig, keys: new WeakMap() };
  const rowKeyOf = (row: number): string | null => {
    const values = cache.get(row);
    if (!values || row >= rowCount || !hasKey) return null;
    const keys = keyCache.current.keys;
    let key = keys.get(values);
    if (key === undefined) {
      key = rowKey(rowIdOf(row)!);
      keys.set(values, key);
    }
    return key;
  };
  const stagedValue = (row: number, col: number): CellValue | typeof DEFAULT | undefined => {
    const name = columns[col]!.name;
    const added = newRowAt(row);
    if (added) return name in added.values ? added.values[name]! : DEFAULT;
    const key = rowKeyOf(row);
    const changed = key ? edits.updates[key]?.values : undefined;
    return changed && name in changed ? changed[name]! : undefined;
  };
  const cellValue = (row: number, col: number): CellValue | typeof DEFAULT | undefined => {
    const staged = stagedValue(row, col);
    return staged !== undefined ? staged : cache.get(row)?.[col];
  };
  const codeFont = useWorkspace((s) => s.settings.codeFont);
  const interfaceFont = useWorkspace((s) => s.settings.interfaceFont);
  const density = useWorkspace((s) => s.settings.density);
  const look = useMemo(() => ({ codeFont, interfaceFont, density }), [codeFont, interfaceFont, density]);
  const rowH = gridRowHeight(codeFont.size, density);
  const headerH = Math.round(HEADER_H * DENSITY_FACTOR[density]);
  const rowNumW = Math.max(ROWNUM_W, String(total).length * codeCharWidth(look) + 14);

  const [measured, setMeasured] = useState<{ look: GridLook; widths: number[] } | null>(null);
  const sampled = Math.min(rowCount, 64);
  const sampleReady = sampled > 0 ? cache.get(sampled - 1) !== undefined : done;
  useEffect(() => {
    if (measured?.look !== look && sampleReady) setMeasured({ look, widths: naturalWidths(columns, cache, sampled, query ? QUERY_CONTROLS : 0, look) });
  }, [measured, look, sampleReady, columns, cache, sampled, query]);
  const [resized, setResized] = useState<Record<number, number>>({});
  const widths = useMemo(() => columns.map((_, i) => resized[i] ?? measured?.widths[i] ?? MIN_COL), [columns, measured, resized]);
  const widthsRef = useRef(widths);
  widthsRef.current = widths;
  const stopResize = useRef<(() => void) | null>(null);
  useEffect(() => () => stopResize.current?.(), []);
  const resizeBy = (col: number, delta: number) =>
    setResized((r) => ({ ...r, [col]: Math.max(RESIZE_MIN, (r[col] ?? widthsRef.current[col] ?? MIN_COL) + delta) }));
  const resetWidth = (col: number) =>
    setResized((r) => {
      const { [col]: _, ...rest } = r;
      return rest;
    });
  const headerActions = useMemo(
    () => ({
      onResizeStart(e: ReactMouseEvent, col: number) {
        e.preventDefault();
        e.stopPropagation();
        stopResize.current?.();
        const startX = e.clientX;
        const startW = widthsRef.current[col]!;
        let next = startW;
        let frame = 0;
        // At most one width change per frame, so a fast drag lays the columns out no more often than the screen draws.
        const apply = () => {
          frame = 0;
          setResized((r) => (r[col] === next ? r : { ...r, [col]: next }));
        };
        const move = (m: MouseEvent) => {
          next = Math.max(RESIZE_MIN, Math.round(startW + m.clientX - startX));
          if (!frame) frame = requestAnimationFrame(apply);
        };
        const stop = () => {
          cancelAnimationFrame(frame);
          window.removeEventListener("mousemove", move);
          window.removeEventListener("mouseup", up);
          stopResize.current = null;
        };
        const up = () => {
          if (frame) apply();
          stop();
        };
        window.addEventListener("mousemove", move);
        window.addEventListener("mouseup", up);
        stopResize.current = stop;
      },
      onResetWidth: (col: number) =>
        setResized((r) => {
          const { [col]: _, ...rest } = r;
          return rest;
        }),
    }),
    [],
  );

  const rows = useVirtualizer({
    count: total,
    getScrollElement: () => scroller.current,
    estimateSize: () => rowH,
    paddingStart: headerH,
    scrollPaddingStart: headerH,
    overscan: 12,
  });
  const cols = useVirtualizer({
    horizontal: true,
    count: columns.length,
    getScrollElement: () => scroller.current,
    estimateSize: (i) => widths[i] ?? MIN_COL,
    paddingStart: rowNumW,
    scrollPaddingStart: rowNumW,
    overscan: 3,
  });
  useLayoutEffect(() => {
    cols.measure();
  }, [cols, widths, rowNumW]);
  useLayoutEffect(() => {
    rows.measure();
  }, [rows, rowH, headerH]);

  const virtualRows = rows.getVirtualItems();
  const firstRow = virtualRows[0]?.index ?? 0;
  const lastRow = virtualRows.at(-1)?.index ?? 0;
  useEffect(() => {
    void cache.ensure({ start: Math.max(0, firstRow - PREFETCH), end: lastRow + 1 + PREFETCH }, rowCount);
  }, [cache, cacheVersion, firstRow, lastRow, rowCount]);

  const onSelectionTotalsRef = useRef(onSelectionTotals);
  onSelectionTotalsRef.current = onSelectionTotals;
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const epochRef = useRef(0);
  const sentSpan = useRef("");
  const mounted = useRef(true);
  const inflightTotals = useRef(0);
  const queuedTotal = useRef<{ epoch: number; rows: RowSpan; cols: ColSpan; pending: boolean } | null>(null);
  const dragPosted = useRef(false);
  const lastPublished = useRef<string | null>(null);
  const resultKey = `${meta.ref.tabId}:${meta.ref.runId}:${meta.ref.index}`;
  const publishTotals = (epoch: number, text: string | null) => {
    lastPublished.current = text;
    onSelectionTotalsRef.current?.({ key: resultKey, epoch, text });
  };
  const syncTotals = (force: boolean) => {
    const sel = selectionRef.current;
    const cells = sel ? visualCellCount(sel) : 0;
    const pending = sel !== null && cells >= 2 && selectionTouchesPending(sel, rowCount, columns, edits, cache, rowKeyOf);
    const span = sel !== null && cells >= 2 ? storedSpan(sel, rowCount) : null;
    const sig =
      cells < 2 || sel === null ? "clear" : span === null ? "inserts" : `${span.rows.start},${span.rows.end},${span.cols.start},${span.cols.end},${pending}`;
    if (dragging.current) {
      queuedTotal.current = null;
      const text = sig === "clear" ? null : sig === "inserts" || pending ? "excludes pending edits" : null;
      if (!dragPosted.current) {
        dragPosted.current = true;
        epochRef.current += 1;
      }
      if (lastPublished.current !== text) publishTotals(epochRef.current, text);
      return;
    }
    dragPosted.current = false;
    if (!force && sentSpan.current === sig) return;
    sentSpan.current = sig;
    const epoch = ++epochRef.current;
    if (sig === "clear") {
      queuedTotal.current = null;
      publishTotals(epoch, null);
      return;
    }
    if (sig === "inserts" || span === null) {
      queuedTotal.current = null;
      publishTotals(epoch, "excludes pending edits");
      return;
    }
    publishTotals(epoch, pending ? "excludes pending edits" : null);
    const job = { epoch, rows: span.rows, cols: span.cols, pending };
    const run = (next: typeof job) => {
      inflightTotals.current += 1;
      void commands
        .resultAggregate(meta.ref, next.rows, next.cols)
        .then(
          (res) => {
            if (mounted.current && res.status === "ok" && next.epoch === epochRef.current) {
              publishTotals(next.epoch, formatSelectionTotals(res.data, next.pending));
            }
          },
          () => {},
        )
        .finally(() => {
          inflightTotals.current -= 1;
          const waiting = queuedTotal.current;
          queuedTotal.current = null;
          if (waiting && mounted.current) run(waiting);
        });
    };
    if (inflightTotals.current >= 2) queuedTotal.current = job;
    else run(job);
  };
  const syncTotalsRef = useRef(syncTotals);
  syncTotalsRef.current = syncTotals;
  useEffect(() => {
    syncTotalsRef.current(false);
  }, [selection, cacheVersion, edits, rowCount, columns]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    const up = () => {
      if (!dragging.current) return;
      dragging.current = null;
      syncTotalsRef.current(true);
    };
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);

  const fullValue = async (row: number, col: number) => {
    const staged = stagedValue(row, col);
    if (staged !== undefined) return staged === DEFAULT ? null : staged;
    return cache.isCut(row, col) ? await cache.loadFull(row, col) : cache.get(row)?.[col];
  };
  const openCell = async (row: number, col: number) => {
    const value = await fullValue(row, col);
    if (value === undefined) return;
    useWorkspace.getState().open({ kind: "cell", title: `${columns[col]?.name ?? ""} · row ${row + 1}`, value });
  };
  const links = useKeyLinks(meta);
  const follow = async (row: number, link: KeyLink) => {
    const values = await Promise.all(link.resultColumns.map((col) => fullValue(row, col)));
    if (!values.every((v) => typeof v === "string")) return;
    await openTableTab({ db: meta.db, schema: link.schema, relation: link.table }, referencedRows(link.refColumns, values));
  };
  const openRecord = (row: number) => {
    if (row >= rowCount) return;
    scroller.current?.focus();
    useWorkspace.getState().open({ kind: "record", meta, row });
  };

  // `draft` is the text the editor opens with. The editor keeps what is typed after that.
  const [editingCell, setEditingCell] = useState<(Cell & { draft: string; initial: string; stored: CellValue }) | null>(null);
  const lastEnded = useRef<typeof editingCell>(null);
  const opening = useRef(0);
  const refused = () => useWorkspace.getState().notify("A submit is running. Try again when it finishes.");
  const canWrite = (col: number) => editable && editing?.writableColumns.has(columns[col]?.name ?? "") === true;
  const startEdit = (row: number, col: number, typed?: string) => {
    const key = rowKeyOf(row);
    if (!canWrite(col) || useTableEdits.getState().submitting[tabId] || (!newRowAt(row) && (!key || key in edits.deletes))) return;
    const name = columns[col]!.name;
    const pending = newRowAt(row) || (key && name in (edits.updates[key]?.values ?? {}));
    const open = (initial: string, stored: CellValue) =>
      setEditingCell((cur) =>
        typed !== undefined && cur?.row === row && cur.col === col
          ? { ...cur, draft: cur.draft + typed }
          : { row, col, initial, stored, draft: typed ?? initial },
      );
    const seq = ++opening.current;
    // The page cache cuts long values. Typing replaces the value, so only opening a cut cell as-is waits for its full text.
    if (pending || typed !== undefined || !cache.isCut(row, col)) {
      const shown = cellValue(row, col);
      return open(typeof shown === "string" ? shown : "", row < rowCount ? (cache.get(row)?.[col] ?? null) : null);
    }
    void cache.loadFull(row, col).then((full) => {
      if (opening.current === seq) open(full ?? "", full ?? null);
    });
  };
  const writeCell = (row: number, col: number, value: CellValue | typeof DEFAULT, stored = cache.get(row)?.[col] ?? null) => {
    if (!canWrite(col)) return;
    const name = columns[col]!.name;
    const added = newRowAt(row);
    if (added) {
      if (!edit((e) => (value === DEFAULT ? resetNewCell(e, added.id, name) : setNewCell(e, added.id, name, value)))) refused();
      return;
    }
    const id = rowIdOf(row);
    if (id && value !== DEFAULT && !edit((e) => setCell(e, id, name, value, stored))) refused();
  };
  const finishEdit = (save: boolean, draft: string) => {
    if (!editingCell || lastEnded.current === editingCell) return;
    lastEnded.current = editingCell;
    if (save && draft !== editingCell.initial) writeCell(editingCell.row, editingCell.col, draft, editingCell.stored);
    setEditingCell(null);
    scroller.current?.focus();
  };
  const tabFromEditor = (back: boolean, draft: string) => {
    if (!editingCell) return;
    const { row, col } = editingCell;
    finishEdit(true, draft);
    const next = { row, col: Math.max(0, Math.min(columns.length - 1, col + (back ? -1 : 1))) };
    setSelection({ anchor: next, focus: next });
    cols.scrollToIndex(next.col);
  };
  const selectedRows = () => {
    if (!selection) return [];
    const r0 = Math.min(selection.anchor.row, selection.focus.row);
    const r1 = Math.max(selection.anchor.row, selection.focus.row);
    return Array.from({ length: r1 - r0 + 1 }, (_, i) => r0 + i);
  };
  const deleting = useRef(false);
  const deleteSelectedRows = async () => {
    if (deleting.current) return;
    const picked = selectedRows();
    const storedRows = picked.filter((r) => r < rowCount);
    deleting.current = true;
    try {
      if (storedRows.length) await cache.ensure({ start: storedRows[0]!, end: storedRows.at(-1)! + 1 }, rowCount);
    } finally {
      deleting.current = false;
    }
    const added = picked.flatMap((r) => newRowAt(r)?.id ?? []);
    const stored = storedRows.flatMap((r) => {
      const id = rowIdOf(r);
      return id ? [id] : [];
    });
    const marked = useTableEdits.getState().byTab[tabId]?.deletes ?? {};
    const restore = stored.length > 0 && stored.every((r) => rowKey(r) in marked);
    if (!edit((e) => markDeleted(removeNewRows(e, added), stored, !restore))) refused();
  };
  const insertsSeen = useRef(edits.inserts.length);
  useEffect(() => {
    if (edits.inserts.length > insertsSeen.current) {
      const row = rowCount + edits.inserts.length - 1;
      const col = Math.max(
        0,
        columns.findIndex((c) => editing?.writableColumns.has(c.name)),
      );
      rows.scrollToIndex(row);
      setSelection({ anchor: { row, col }, focus: { row, col } });
      scroller.current?.focus();
    }
    insertsSeen.current = edits.inserts.length;
  }, [edits.inserts.length, rowCount, columns, editing, rows]);
  useImperativeHandle<GridActions | null, GridActions | null>(actionsRef, () => {
    if (!editable) return null;
    return { addRow: () => void useTableEdits.getState().addRow(tabId), deleteSelectedRows: () => void deleteSelectedRows() };
  });

  const openMenuAt = (cell: Cell) => {
    const open = () => {
      const target = document.getElementById(cellId(cell.row, cell.col));
      if (!target) return;
      const box = target.getBoundingClientRect();
      target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: box.left + 4, clientY: box.bottom }));
    };
    if (document.getElementById(cellId(cell.row, cell.col))) return open();
    rows.scrollToIndex(cell.row);
    cols.scrollToIndex(cell.col);
    requestAnimationFrame(open);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const mod = e.metaKey || e.ctrlKey;
    if (editable && mod && e.key === "Backspace" && selection) {
      e.preventDefault();
      if (!e.repeat) void deleteSelectedRows();
    } else if (editable && selection && (e.key === "Enter" || e.key === "F2")) {
      e.preventDefault();
      startEdit(selection.focus.row, selection.focus.col);
    } else if (editable && selection && e.key.length === 1 && !mod && !e.altKey) {
      e.preventDefault();
      startEdit(selection.focus.row, selection.focus.col, e.key);
    } else if (mod && e.key.toLowerCase() === "c" && selection) {
      e.preventDefault();
      void copySelection(cache, selection, rowCount, total, (r, c) => {
        const staged = stagedValue(r, c);
        return staged === DEFAULT ? null : staged;
      });
    } else if (((e.shiftKey && e.key === "F10") || e.key === "ContextMenu") && selection) {
      e.preventDefault();
      openMenuAt(selection.focus);
    } else if (mod && e.key.toLowerCase() === "i" && selection) {
      e.preventDefault();
      openRecord(selection.focus.row);
    } else if (mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      setSelection({ anchor: { row: 0, col: 0 }, focus: { row: total - 1, col: columns.length - 1 } });
    } else if (e.altKey && !mod && (e.key === "ArrowLeft" || e.key === "ArrowRight") && selection) {
      // ⌥← and ⌥→ resize the selected cell's column, the keyboard's way to drag its header's edge.
      e.preventDefault();
      resizeBy(selection.focus.col, e.key === "ArrowRight" ? RESIZE_STEP : -RESIZE_STEP);
    } else if (query && e.altKey && !mod && (e.code === "KeyS" || e.key.toLowerCase() === "s") && selection) {
      // ⌥S sorts by the selected cell's column as a header click does, and ⇧⌥S adds it to the sort as a Shift-click does.
      e.preventDefault();
      query.onSort(columns[selection.focus.col]!.name, e.shiftKey);
    } else if (e.key === "Enter" && selection) {
      void openCell(selection.focus.row, selection.focus.col);
    } else {
      const focus = nextFocus(e.key, mod, selection?.focus ?? { row: 0, col: 0 }, total, columns.length);
      if (!focus) return;
      e.preventDefault();
      rows.scrollToIndex(focus.row);
      cols.scrollToIndex(focus.col);
      setSelection((sel) => ({ anchor: e.shiftKey && sel ? sel.anchor : focus, focus }));
    }
  };

  // The cells carry no handlers. These read the cell or row number an event landed on.
  const onMouseDown = (e: ReactMouseEvent) => {
    if (e.button !== 0 || (e.target as Element).closest("[data-cut]")) return;
    const cell = cellAt(e.target);
    if (cell) {
      dragging.current = "cells";
      setSelection((sel) => ({ anchor: e.shiftKey && sel ? sel.anchor : cell, focus: cell }));
      return;
    }
    const row = rowNumberAt(e.target);
    if (row === null) return;
    dragging.current = "rows";
    setSelection((sel) => selectRows(e.shiftKey && sel ? sel.anchor.row : row, row, columns.length));
  };
  const onMouseOver = (e: ReactMouseEvent) => {
    if (!dragging.current) return;
    const cell = cellAt(e.target);
    if (dragging.current === "cells") {
      if (cell) setSelection((sel) => (sel && (sel.focus.row !== cell.row || sel.focus.col !== cell.col) ? { ...sel, focus: cell } : sel));
      return;
    }
    const row = cell?.row ?? rowNumberAt(e.target);
    if (row !== null) setSelection((sel) => (sel && sel.focus.row !== row ? selectRows(sel.anchor.row, row, columns.length) : sel));
  };

  const findInput = useRef<HTMLInputElement>(null);
  const find = useResultFind(meta.ref, rowCount, (cell) => {
    if (document.activeElement !== findInput.current) return;
    rows.scrollToIndex(cell.row);
    cols.scrollToIndex(cell.col);
    setSelection({ anchor: cell, focus: cell });
  });
  const showFind = () => {
    ++opening.current;
    find.show();
    requestAnimationFrame(() => {
      findInput.current?.focus();
      findInput.current?.select();
    });
  };
  const closeFind = () => {
    find.close();
    scroller.current?.focus();
  };
  const activeMatch = find.state.kind === "ready" ? find.state.result.target?.cell : undefined;
  const matchQuery = find.open ? find.query : "";

  const virtualCols = cols.getVirtualItems();
  const focus = selection?.focus;
  const bounds = selection ? selectionBounds(selection) : null;
  const fullRows = fullRowRange(selection, columns.length);
  const gridId = useId();
  const cellId = (row: number, col: number) => `${gridId}-${row}-${col}`;
  const focusRendered = focus && virtualRows.some((vr) => vr.index === focus.row) && virtualCols.some((vc) => vc.index === focus.col);
  const [menuCell, setMenuCell] = useState<Cell | null>(null);
  const menuColumn = menuCell ? columns[menuCell.col]?.name : undefined;
  const menuValue = menuCell && menuCell.row < rowCount ? cache.get(menuCell.row)?.[menuCell.col] : undefined;
  const menuRowKey = menuCell ? rowKeyOf(menuCell.row) : null;
  const menuRowDeleted = menuRowKey ? menuRowKey in edits.deletes : false;
  const shortValue = (v: string) => (v.length > 24 ? `${v.slice(0, 24)}…` : v);
  const menuLinks = menuCell ? links.filter((link) => link.resultColumns.includes(menuCell.col)) : [];
  const linkLabel = (link: KeyLink) => {
    const sameTable = menuLinks.filter((other) => other.schema === link.schema && other.table === link.table).length > 1;
    return `Go to referenced row in ${link.table}${sameTable ? ` (${link.columns.join(", ")})` : ""}`;
  };
  const hasValue = (row: number, col: number) => typeof cellValue(row, col) === "string";
  const editor = editingCell && (
    <GridEditor column={columns[editingCell.col]?.name ?? ""} initial={editingCell.draft} onFinish={finishEdit} onTab={tabFromEditor} />
  );

  const grid = (
    <div
      ref={scroller}
      data-testid="result-grid"
      data-row-count={rowCount}
      data-pending={edits.inserts.length + Object.keys(edits.updates).length + Object.keys(edits.deletes).length}
      tabIndex={0}
      role="grid"
      aria-label="Result"
      aria-rowcount={total + 1}
      aria-colcount={columns.length + 1}
      aria-multiselectable
      aria-readonly={!editable}
      aria-activedescendant={focusRendered ? cellId(focus.row, focus.col) : undefined}
      onKeyDown={onKeyDown}
      onMouseDown={onMouseDown}
      onMouseOver={onMouseOver}
      onClick={(e) => {
        const cell = (e.target as Element).closest("[data-cut]") ? cellAt(e.target) : null;
        if (cell) void openCell(cell.row, cell.col);
      }}
      onDoubleClick={(e) => {
        const cell = cellAt(e.target);
        if (cell) void (canWrite(cell.col) ? startEdit(cell.row, cell.col) : openCell(cell.row, cell.col));
      }}
      onContextMenu={(e) => {
        const cell = cellAt(e.target);
        if (!cell) return e.preventDefault();
        setMenuCell(cell);
        if (!inSelection(selection, cell.row, cell.col)) setSelection({ anchor: cell, focus: cell });
      }}
      // The selected cell draws its own ring, so the grid shows the shared focus ring only while no cell on screen is selected.
      className={cx("relative min-h-0 flex-1 overflow-auto bg-bg", focusRendered && "outline-none")}
    >
      <div role="presentation" style={{ width: `max(${cols.getTotalSize()}px, 100%)`, height: rows.getTotalSize(), position: "relative" }}>
        <GridHeader
          columns={columns}
          virtualCols={virtualCols}
          pk={pk}
          focusCol={focus?.col ?? -1}
          query={query}
          editing={editing}
          height={headerH}
          rowNumW={rowNumW}
          onResizeStart={headerActions.onResizeStart}
          onResetWidth={headerActions.onResetWidth}
        />
        {virtualRows.map((vr) => {
          const index = vr.index;
          const added = newRowAt(index);
          const key = added ? null : rowKeyOf(index);
          const deleted = key ? key in edits.deletes : false;
          const failed = !!edits.failedAt && ("insert" in edits.failedAt ? added?.id === edits.failedAt.insert : key === edits.failedAt.key);
          const selectedHere = bounds !== null && index >= bounds.r0 && index <= bounds.r1;
          return (
            <GridRow
              findQuery={matchQuery}
              activeMatch={activeMatch}
              key={vr.key}
              index={index}
              top={vr.start}
              height={rowH}
              rowNumW={rowNumW}
              idPrefix={gridId}
              columns={columns}
              virtualCols={virtualCols}
              cache={cache}
              values={cache.get(index)}
              added={added}
              deleted={deleted}
              changed={key ? edits.updates[key]?.values : undefined}
              failed={failed}
              selFrom={selectedHere ? bounds.c0 : -1}
              selTo={selectedHere ? bounds.c1 : -1}
              focusCol={focus?.row === index ? focus.col : -1}
              fullRow={fullRows !== null && index >= fullRows[0] && index <= fullRows[1]}
              editingCol={editingCell?.row === index ? editingCell.col : -1}
              editor={editingCell?.row === index ? editor : null}
            />
          );
        })}
      </div>
    </div>
  );
  return (
    <ContextMenuArea
      menu={
        menuCell &&
        menuColumn !== undefined && (
          <>
            {query && (
              <>
                <ContextMenuItem testId="grid-menu-sort" keyShortcuts="Alt+S" onSelect={() => query.onSort(menuColumn, false)}>
                  Sort by {menuColumn}
                  <KeyHint>{ALT_KEY}S</KeyHint>
                </ContextMenuItem>
                {menuValue !== undefined && (
                  <>
                    <ContextMenuItem testId="grid-menu-filter-eq" onSelect={() => query.onFilterValue(menuColumn, menuValue, false)}>
                      {menuValue === null ? `Filter ${menuColumn} is null` : `Filter ${menuColumn} = ${shortValue(menuValue)}`}
                    </ContextMenuItem>
                    <ContextMenuItem testId="grid-menu-filter-ne" onSelect={() => query.onFilterValue(menuColumn, menuValue, true)}>
                      {menuValue === null ? `Filter ${menuColumn} is not null` : `Filter ${menuColumn} ≠ ${shortValue(menuValue)}`}
                    </ContextMenuItem>
                  </>
                )}
                <ContextMenuItem testId="grid-menu-filter-column" onSelect={() => query.onFilterColumn(menuColumn)}>
                  Filter {menuColumn}…
                </ContextMenuItem>
                <ContextMenuSeparator />
              </>
            )}
            {menuLinks.map((link) => (
              <ContextMenuItem
                key={`${link.name}:${link.resultColumns.join(",")}`}
                testId={`grid-menu-follow-${link.name}`}
                disabled={!link.resultColumns.every((col) => hasValue(menuCell.row, col))}
                onSelect={() => void follow(menuCell.row, link)}
              >
                {linkLabel(link)}
              </ContextMenuItem>
            ))}
            <ContextMenuItem testId="grid-menu-record" keyShortcuts="Meta+I" disabled={menuCell.row >= rowCount} onSelect={() => openRecord(menuCell.row)}>
              View as record
              <KeyHint>{MOD_KEY}I</KeyHint>
            </ContextMenuItem>
            <ContextMenuSeparator />
            <ContextMenuItem testId="grid-menu-widen" keyShortcuts="Alt+ArrowRight" onSelect={() => resizeBy(menuCell.col, RESIZE_STEP)}>
              Widen column
              <KeyHint>{ALT_KEY}→</KeyHint>
            </ContextMenuItem>
            <ContextMenuItem testId="grid-menu-narrow" keyShortcuts="Alt+ArrowLeft" onSelect={() => resizeBy(menuCell.col, -RESIZE_STEP)}>
              Narrow column
              <KeyHint>{ALT_KEY}←</KeyHint>
            </ContextMenuItem>
            {resized[menuCell.col] !== undefined && (
              <ContextMenuItem testId="grid-menu-reset-width" onSelect={() => resetWidth(menuCell.col)}>
                Reset column width
              </ContextMenuItem>
            )}
            {editable && (
              <>
                <ContextMenuSeparator />
                {!menuRowDeleted && canWrite(menuCell.col) && (
                  <>
                    <ContextMenuItem testId="grid-menu-edit" onSelect={() => startEdit(menuCell.row, menuCell.col)}>
                      Edit cell
                    </ContextMenuItem>
                    <ContextMenuItem testId="grid-menu-null" onSelect={() => writeCell(menuCell.row, menuCell.col, null)}>
                      Set NULL
                    </ContextMenuItem>
                    {newRowAt(menuCell.row) && (
                      <ContextMenuItem testId="grid-menu-default" onSelect={() => writeCell(menuCell.row, menuCell.col, DEFAULT)}>
                        Set DEFAULT
                      </ContextMenuItem>
                    )}
                  </>
                )}
                <ContextMenuItem testId="grid-menu-delete" onSelect={deleteSelectedRows}>
                  {menuRowDeleted ? "Restore row" : "Delete row"}
                </ContextMenuItem>
                <ContextMenuItem testId="grid-menu-add" onSelect={() => useTableEdits.getState().addRow(tabId)}>
                  Add row
                </ContextMenuItem>
              </>
            )}
          </>
        )
      }
    >
      <div
        className="flex min-h-0 flex-1 flex-col"
        onKeyDownCapture={(e) => {
          if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "f") {
            e.preventDefault();
            if (e.target instanceof HTMLInputElement && e.target !== findInput.current) e.target.blur();
            e.stopPropagation();
            showFind();
          }
        }}
      >
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-grid px-2 py-1 text-ui-12 text-muted">
          {find.open ? (
            <>
              <input
                ref={findInput}
                aria-label="Find in results"
                value={find.query}
                onChange={(e) => find.change(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    closeFind();
                  }
                  if (e.key === "Enter") {
                    e.preventDefault();
                    find.navigate(e.shiftKey);
                  }
                }}
                className="min-w-0 rounded border border-grid bg-bg px-2 py-1 text-fg"
              />
              <span role="status">
                {find.state.kind === "failed"
                  ? find.state.message
                  : find.state.kind === "ready"
                    ? `${find.state.result.target?.ordinal ?? 0} of ${find.state.result.count} matching cells`
                    : "Searching…"}
              </span>
              <IconButton
                aria-label="Previous matching cell"
                title="Previous matching cell (Shift+Enter)"
                disabled={find.state.kind !== "ready" || !find.state.result.target}
                onClick={() => {
                  find.navigate(true);
                  findInput.current?.focus();
                }}
              >
                <ChevronUpIcon size={15} />
              </IconButton>
              <IconButton
                aria-label="Next matching cell"
                title="Next matching cell (Enter)"
                disabled={find.state.kind !== "ready" || !find.state.result.target}
                onClick={() => {
                  find.navigate(false);
                  findInput.current?.focus();
                }}
              >
                <ChevronDownIcon size={15} />
              </IconButton>
              <IconButton aria-label="Close find" title="Close find (Escape)" onClick={closeFind}>
                <XIcon size={15} />
              </IconButton>
            </>
          ) : (
            <button onClick={showFind} title={`${MOD_KEY}F`}>
              Find in results
            </button>
          )}
          <span>
            {query ? "Current table page" : "Current result"}
            {meta.truncated ? " · Result truncated" : ""}
            {edits.inserts.length || Object.keys(edits.updates).length || Object.keys(edits.deletes).length ? " · Saved/retained values only" : ""}
          </span>
        </div>
        {grid}
      </div>
    </ContextMenuArea>
  );
});

function KeyHint({ children }: { children: string[] | string }) {
  return (
    <span aria-hidden className="ml-auto pl-6 text-ui-12 text-dim">
      {children}
    </span>
  );
}
