import type { VirtualItem } from "@tanstack/react-virtual";
import { memo, type ReactNode } from "react";
import type { ColumnMeta, ResultRef } from "../../ipc/bindings";
import type { Row, RowView } from "../../state/rowCache";
import type { CellValue, NewRow } from "../../state/tableEdits";
import { cx } from "../../ui/primitives";
import { storedValueMatches, useCutCellMatch } from "./useResultFind";
import { groupDigits, isNumeric } from "./format";

export const DEFAULT = Symbol("default");

export type Cell = { row: number; col: number };

const VALUE_MARKS = {
  default: { text: "DEFAULT", label: undefined, border: "" },
  null: { text: "NULL", label: undefined, border: "border" },
  empty: { text: "empty", label: "Empty string", border: "border border-dashed" },
} as const;

export const NULL_MARK = VALUE_MARKS.null.text;
export const EMPTY_MARK = VALUE_MARKS.empty.text;

function ValueMark({ kind, deleted }: { kind: keyof typeof VALUE_MARKS; deleted: boolean }) {
  const mark = VALUE_MARKS[kind];
  return (
    <span
      role={mark.label ? "img" : undefined}
      aria-label={mark.label}
      className={cx("rounded-[3px] border-faint px-1 font-sans text-ui-11 leading-4 italic", mark.border, !deleted && "text-dim")}
    >
      {mark.text}
    </span>
  );
}

/** Reads the cell a grid event landed on from the cell's `data-row` and `data-col`, so cells carry no handlers of their own. */
export function cellAt(target: EventTarget): Cell | null {
  const el = target instanceof Element ? target.closest<HTMLElement>("[data-col]") : null;
  return el ? { row: Number(el.dataset.row), col: Number(el.dataset.col) } : null;
}

export function rowNumberAt(target: EventTarget): number | null {
  const el = target instanceof Element ? target.closest<HTMLElement>("[data-rownum]") : null;
  return el ? Number(el.dataset.rownum) : null;
}

type CellProps = {
  resultRef: ResultRef;
  findQuery: string;
  id: string;
  row: number;
  storedRow: number;
  col: number;
  left: number;
  width: number;
  value: CellValue | typeof DEFAULT | undefined;
  right: boolean;
  cut: boolean;
  matched: boolean;
  selected: boolean;
  focused: boolean;
  changed: boolean;
  deleted: boolean;
  editor: ReactNode;
};

const GridCell = memo(function GridCell({
  resultRef,
  findQuery,
  id,
  row,
  storedRow,
  col,
  left,
  width,
  value,
  right,
  cut,
  matched,
  selected,
  focused,
  changed,
  deleted,
  editor,
}: CellProps) {
  const suffixMatch = useCutCellMatch(resultRef, storedRow, col, cut && !matched ? findQuery : "");
  const findMatch = matched || suffixMatch;
  return (
    <div
      data-testid={`grid-cell-${row}-${col}`}
      data-row={row}
      data-col={col}
      id={id}
      role="gridcell"
      aria-colindex={col + 2}
      data-find-match={findMatch || undefined}
      aria-selected={selected}
      className={cx(
        "absolute top-0 flex h-full items-center gap-1 border-r border-grid px-2 font-mono text-code",
        right && "justify-end",
        changed && "bg-amber/15",
        selected && "bg-cell-select",
        findMatch && "ring-2 ring-inset ring-amber",
        deleted && "text-dim line-through",
        focused && "outline-2 -outline-offset-2 outline-focus outline-solid",
      )}
      style={{ left, width }}
    >
      {editor ? (
        editor
      ) : value === DEFAULT ? (
        <ValueMark kind="default" deleted={deleted} />
      ) : value === undefined ? null : value === null ? (
        <ValueMark kind="null" deleted={deleted} />
      ) : value === "" ? (
        <ValueMark kind="empty" deleted={deleted} />
      ) : (
        <>
          <span className={cx("min-w-0 truncate whitespace-pre", !deleted && "text-fg")}>{right ? groupDigits(value) : value.replace(/\n/g, "↵ ")}</span>
          {cut && (
            <button
              type="button"
              data-testid={`grid-cut-${row}-${col}`}
              data-cut
              aria-label="Show the full value"
              title="Show the full value"
              className="shrink-0 rounded bg-control px-1 font-sans text-ui-11 leading-4 text-muted hover:text-fg"
            >
              …
            </button>
          )}
        </>
      )}
      {changed && <span className="sr-only">, changed</span>}
    </div>
  );
});

export type RowProps = {
  findQuery?: string;
  activeMatch?: Cell;
  index: number;
  rowOffset: number;
  top: number;
  height: number;
  rowNumW: number;
  idPrefix: string;
  columns: ColumnMeta[];
  virtualCols: VirtualItem[];
  cache: RowView;
  /** The stored values; a page that loads again brings new arrays, and with them its cut cells. */
  values: Row | undefined;
  added: NewRow | undefined;
  deleted: boolean;
  changed: Record<string, CellValue> | undefined;
  failed: boolean;
  /** The selected columns in this row, or -1 when the selection misses it. */
  selFrom: number;
  selTo: number;
  focusCol: number;
  fullRow: boolean;
  editingCol: number;
  editor: ReactNode;
};

export const GridRow = memo(function GridRow({
  findQuery = "",
  activeMatch,
  index,
  rowOffset,
  top,
  height,
  rowNumW,
  idPrefix,
  columns,
  virtualCols,
  cache,
  values,
  added,
  deleted,
  changed,
  failed,
  selFrom,
  selTo,
  focusCol,
  fullRow,
  editingCol,
  editor,
}: RowProps) {
  const background = added
    ? "bg-[color-mix(in_srgb,var(--color-green)_10%,var(--color-bg))]"
    : deleted
      ? "bg-[color-mix(in_srgb,var(--color-danger)_10%,var(--color-bg))]"
      : changed
        ? "bg-[color-mix(in_srgb,var(--color-amber)_8%,var(--color-bg))]"
        : "hover:bg-hover";
  return (
    <div
      data-testid={`grid-row-${index}`}
      role="row"
      aria-rowindex={rowOffset + index + 2}
      data-state={added ? "new" : deleted ? "deleted" : changed ? "changed" : undefined}
      className={cx("group absolute right-0 left-0 border-b border-grid", background, failed && "outline outline-1 -outline-offset-1 outline-danger")}
      style={{ top, height }}
    >
      <div
        data-testid={`grid-rownum-${index}`}
        data-rownum={index}
        role="rowheader"
        aria-colindex={1}
        className={cx(
          "sticky left-0 z-10 flex h-full items-center justify-center border-r border-grid font-mono text-code-sm tabular-nums",
          fullRow
            ? "bg-cell-select text-fg"
            : focusCol >= 0
              ? "bg-hover text-fg"
              : added || deleted || changed
                ? "bg-inherit text-dim"
                : "bg-bg text-dim group-hover:bg-hover",
        )}
        style={{ width: rowNumW }}
      >
        {added ? (
          <>
            <span aria-hidden className="text-green">
              +
            </span>
            <span className="sr-only">New row</span>
          </>
        ) : (
          <>
            {rowOffset + index + 1}
            {deleted ? <span className="sr-only">, marked for deletion</span> : changed && <span className="sr-only">, changed</span>}
          </>
        )}
        {failed && <span className="sr-only">, failed to save</span>}
      </div>
      {virtualCols.map((vc) => {
        const name = columns[vc.index]!.name;
        const staged = added ? (name in added.values ? added.values[name]! : DEFAULT) : changed && name in changed ? changed[name]! : undefined;
        return (
          <GridCell
            key={vc.key}
            resultRef={cache.ref}
            findQuery={findQuery}
            id={`${idPrefix}-${index}-${vc.index}`}
            row={index}
            storedRow={rowOffset + index}
            col={vc.index}
            left={vc.start}
            width={vc.size}
            value={staged !== undefined ? staged : values?.[vc.index]}
            right={isNumeric(columns[vc.index]!.type.category)}
            cut={staged === undefined && !added && cache.isCut(index, vc.index)}
            matched={
              staged === undefined &&
              !added &&
              (storedValueMatches(values?.[vc.index], findQuery) || (activeMatch?.row === index && activeMatch.col === vc.index))
            }
            selected={vc.index >= selFrom && vc.index <= selTo}
            focused={focusCol === vc.index}
            changed={!added && changed !== undefined && name in changed}
            deleted={deleted}
            editor={editingCol === vc.index ? editor : null}
          />
        );
      })}
    </div>
  );
});
