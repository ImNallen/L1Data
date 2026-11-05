import { defaultRangeExtractor, useVirtualizer, type Range, type VirtualItem } from "@tanstack/react-virtual";
import { ChevronRightIcon } from "lucide-react";
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FocusEvent,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { ContextMenuArea, cx } from "../../ui/primitives";
import { FOCUS_FIRST_ROW, focusRow } from "./treeFocus";
import { treeKey, typeAhead, type TreeItem } from "./treeKeys";
import { useIsPickerOpen, type PickerTarget } from "./VisibilityPopover";

export const INDENT = 12;

/** A connection whose rows share one shaded block while tinting is on. Every row of the connection holds the same object. */
export type TintGroup = { name: string; hex: string };

export type RowView = {
  icon?: ReactNode;
  label: ReactNode;
  trailing?: ReactNode;
  /** A muted line under the label, cut with an ellipsis and whole in its title. */
  second?: string;
  title?: string;
};

type NodeBase = { key: string; depth: number; tint: TintGroup | null };

/** A tree row. Its `key` is its path, which names it in `tree-node-<path>`, unless `path` sets that name apart from the key. */
export type RowNode = NodeBase & {
  kind: "row";
  /** Overloaded routines share a path, so their keys add the OID to stay unique while the test id keeps the path. */
  path?: string;
  /** What type-ahead matches. */
  name: string;
  expandable: boolean;
  open?: boolean;
  /** Set only on rows that can be selected, so the others carry no `aria-selected`. */
  selected?: boolean;
  quietChevron?: boolean;
  keepChevronSpace?: boolean;
  twoLines?: boolean;
  /** The picker the row's pill opens. The row shades while it is open, and stays rendered so the picker keeps its anchor. */
  picker?: PickerTarget;
  /** Built only for the rows on screen. `active` is true while the row's picker is open. */
  view: (active: boolean) => RowView;
  menu?: () => ReactNode;
  onClick?: () => void;
  onDoubleClick?: () => void;
  onToggle?: () => void;
};

/** A line that is not a row: a failure, a loading note. */
export type NoteNode = NodeBase & { kind: "note"; size: number; render: () => ReactNode };

export type TreeNode = RowNode | NoteNode;

const ROW_H = 28;
const TWO_LINE_H = 44;
const GROUP_GAP = 4;
const TYPE_AHEAD_MS = 500;
/** Enough rows for a tall sidebar before the first measure, and for static rendering, which never measures. */
const FIRST_RECT = { width: 0, height: 1000 };

/** Where each row sits among its siblings, and the index tables the keyboard walks. */
type Shape = {
  /** Node index of each row, in order. */
  rows: number[];
  items: TreeItem[];
  names: string[];
  ordinal: Map<number, number>;
  index: Map<string, number>;
  setSize: number[];
  posInSet: number[];
  groups: Map<TintGroup, { first: number; last: number }>;
};

function shapeOf(nodes: readonly TreeNode[]): Shape {
  const shape: Shape = { rows: [], items: [], names: [], ordinal: new Map(), index: new Map(), setSize: [], posInSet: [], groups: new Map() };
  const parents: { level: number; key: string }[] = [];
  const siblings = new Map<string, number>();
  const parentOf: string[] = [];
  nodes.forEach((node, i) => {
    shape.index.set(node.key, i);
    if (node.tint) {
      const span = shape.groups.get(node.tint);
      if (span) span.last = i;
      else shape.groups.set(node.tint, { first: i, last: i });
    }
    if (node.kind !== "row") return;
    const level = node.depth + 1;
    while ((parents.at(-1)?.level ?? 0) >= level) parents.pop();
    const parent = parents.at(-1)?.key ?? "";
    parents.push({ level, key: node.key });
    parentOf[i] = parent;
    shape.posInSet[i] = (siblings.get(parent) ?? 0) + 1;
    siblings.set(parent, shape.posInSet[i]);
    shape.ordinal.set(i, shape.rows.length);
    shape.rows.push(i);
    shape.items.push({ level, expandable: node.expandable, expanded: node.expandable && node.open === true });
    shape.names.push(node.name);
  });
  for (const i of shape.rows) shape.setSize[i] = siblings.get(parentOf[i] ?? "") ?? 1;
  return shape;
}

const rowToggle = (node: RowNode) => (node.expandable ? (node.onToggle ?? node.onClick) : undefined);

type RowProps = { node: RowNode; setSize: number; posInSet: number; tabStop: boolean; menuOpen: boolean };

function RowBody({ node, setSize, posInSet, tabStop, menuOpen, active }: RowProps & { active: boolean }) {
  const { icon, label, trailing, second, title } = node.view(active);
  const twoLines = second !== undefined;
  const { onToggle } = node;
  return (
    <div
      data-testid={`tree-node-${node.path ?? node.key}`}
      role="treeitem"
      aria-expanded={node.expandable ? node.open : undefined}
      aria-selected={node.selected}
      aria-level={node.depth + 1}
      aria-setsize={setSize}
      aria-posinset={posInSet}
      title={title}
      aria-description={title}
      tabIndex={tabStop ? 0 : -1}
      onClick={node.onClick}
      onDoubleClick={node.onDoubleClick}
      style={{ paddingLeft: 8 + node.depth * INDENT }}
      className={cx(
        "group flex cursor-default gap-1.5 rounded-md pr-2.5",
        twoLines ? "items-start py-1" : "h-7 items-center",
        node.selected ? "bg-cell-select" : active ? "bg-selected" : cx("hover:bg-hover", menuOpen && "group-data-[state=open]/tree:bg-hover"),
      )}
    >
      <span
        onClick={
          onToggle &&
          ((e) => {
            e.stopPropagation();
            onToggle();
          })
        }
        onDoubleClick={onToggle && ((e) => e.stopPropagation())}
        className={cx("shrink-0 place-items-center self-stretch text-muted", node.expandable || node.keepChevronSpace ? "grid w-3.5" : "hidden")}
      >
        {node.expandable && (
          <ChevronRightIcon
            size={14}
            className={cx("transition-transform duration-100", node.open && "rotate-90", node.quietChevron && !node.open && "invisible group-hover:visible")}
          />
        )}
      </span>
      {icon && <span className={cx("grid w-[18px] shrink-0 place-items-center", twoLines && "h-5")}>{icon}</span>}
      {twoLines ? (
        <span className="min-w-0 flex-1">
          <span className="flex h-5 items-center">
            <span className="min-w-0 flex-1 truncate text-ui-14 text-tree">{label}</span>
            {trailing}
          </span>
          <span title={second} className="block truncate font-mono text-ui-11 leading-4 text-muted">
            {second}
          </span>
        </span>
      ) : (
        <>
          <span className="min-w-0 flex-1 truncate text-ui-14 text-tree">{label}</span>
          {trailing}
        </>
      )}
    </div>
  );
}

/** A row with a picker pill: it alone subscribes to the picker, and while the picker is open it asks to stay rendered. */
function PickerRow({ picker, onPin, ...props }: RowProps & { picker: PickerTarget; onPin: (key: string, on: boolean) => void }) {
  const active = useIsPickerOpen(picker);
  const { key } = props.node;
  useLayoutEffect(() => {
    if (!active) return;
    onPin(key, true);
    return () => onPin(key, false);
  }, [active, key, onPin]);
  return <RowBody {...props} active={active} />;
}

type LineProps = Omit<RowProps, "node"> & {
  node: TreeNode;
  index: number;
  top: number;
  inTint: boolean;
  gapAfter: boolean;
  measure: (el: HTMLElement | null) => void;
  onPin: (key: string, on: boolean) => void;
};

/** One positioned line. Memoized, so a scroll or a focus move re-renders only the lines whose props changed. */
const Line = memo(function Line({ node, index, top, inTint, gapAfter, measure, onPin, ...row }: LineProps) {
  return (
    <div
      ref={measure}
      data-index={index}
      className={cx("absolute top-0", inTint ? "inset-x-0" : "inset-x-2", gapAfter && "pb-1")}
      style={{ transform: `translateY(${top}px)` }}
    >
      {node.kind === "note" ? (
        node.render()
      ) : node.picker ? (
        <PickerRow {...row} node={node} picker={node.picker} onPin={onPin} />
      ) : (
        <RowBody {...row} node={node} active={false} />
      )}
    </div>
  );
});

/**
 * The tree's rows, flattened and virtualized: only the rows on screen are in the DOM, plus the row that holds the Tab
 * stop and the row whose picker is open. The keyboard walks the flat list, so it reaches rows scrolled out of view.
 * One context menu serves every row.
 */
export function VirtualTree({ nodes, scrollRef }: { nodes: readonly TreeNode[]; scrollRef: RefObject<HTMLDivElement | null> }) {
  const shape = useMemo(() => shapeOf(nodes), [nodes]);
  const [focusKey, setFocusKey] = useState<string | null>(null);
  const [pinned, setPinned] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ key: string; content: ReactNode } | null>(null);
  const pendingFocus = useRef<string | null>(null);
  const typed = useRef({ text: "", at: 0 });

  const focused = focusKey === null ? undefined : shape.index.get(focusKey);
  const tabStop = focused !== undefined && nodes[focused]?.kind === "row" ? focused : shape.rows[0];
  const pin = pinned === null ? undefined : shape.index.get(pinned);
  const rangeExtractor = useCallback(
    (range: Range) => {
      const shown = defaultRangeExtractor(range);
      const extra = [tabStop, pin].filter((i): i is number => i !== undefined && i < range.count && !shown.includes(i));
      return extra.length === 0 ? shown : [...new Set([...shown, ...extra])].sort((a, b) => a - b);
    },
    [tabStop, pin],
  );
  const estimateSize = useCallback(
    (i: number) => {
      const node = nodes[i];
      const size = !node ? ROW_H : node.kind === "note" ? node.size : node.twoLines ? TWO_LINE_H : ROW_H;
      return node?.tint && shape.groups.get(node.tint)?.last === i ? size + GROUP_GAP : size;
    },
    [nodes, shape],
  );
  const getItemKey = useCallback((i: number) => nodes[i]?.key ?? i, [nodes]);
  const virtualizer = useVirtualizer({
    count: nodes.length,
    getScrollElement: () => scrollRef.current,
    estimateSize,
    getItemKey,
    rangeExtractor,
    paddingStart: 8,
    paddingEnd: 12,
    overscan: 8,
    initialRect: FIRST_RECT,
  });

  const rowElement = useCallback((i: number) => scrollRef.current?.querySelector<HTMLElement>(`[data-index="${i}"] > [role="treeitem"]`) ?? null, [scrollRef]);
  const focusNode = useCallback(
    (i: number) => {
      const key = nodes[i]?.key;
      if (key === undefined) return;
      const row = rowElement(i);
      if (row) return focusRow(row);
      // Off screen: make it the Tab stop, which renders it, then focus it once it is in the DOM.
      pendingFocus.current = key;
      setFocusKey(key);
    },
    [nodes, rowElement],
  );
  useLayoutEffect(() => {
    const key = pendingFocus.current;
    const i = key === null ? undefined : shape.index.get(key);
    if (i === undefined) return;
    const row = rowElement(i);
    if (!row) return;
    pendingFocus.current = null;
    focusRow(row);
  });

  useEffect(() => {
    const el = scrollRef.current;
    const first = shape.rows[0];
    if (!el || first === undefined) return;
    const onFocusFirst = () => focusNode(first);
    el.addEventListener(FOCUS_FIRST_ROW, onFocusFirst);
    return () => el.removeEventListener(FOCUS_FIRST_ROW, onFocusFirst);
  }, [scrollRef, shape, focusNode]);

  const onPin = useCallback((key: string, on: boolean) => setPinned((p) => (on ? key : p === key ? null : p)), []);

  const nodeAt = (el: EventTarget | null): number | undefined => {
    const line = el instanceof Element ? el.closest<HTMLElement>("[data-index]") : null;
    return line ? Number(line.dataset.index) : undefined;
  };

  const onFocus = (e: FocusEvent<HTMLDivElement>) => {
    if (e.target.getAttribute("role") !== "treeitem") return;
    const i = nodeAt(e.target);
    const key = i === undefined ? undefined : nodes[i]?.key;
    if (key !== undefined) setFocusKey(key);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    if (target.getAttribute("role") !== "treeitem" || e.metaKey || e.ctrlKey || e.altKey) return;
    const i = nodeAt(target);
    if (i === undefined) return;
    const at = shape.ordinal.get(i);
    const node = nodes[i];
    if (at === undefined || node?.kind !== "row") return;

    const t = typed.current;
    if (e.key.length === 1 && (e.key !== " " || t.text !== "")) {
      t.text = e.timeStamp - t.at < TYPE_AHEAD_MS ? t.text + e.key : e.key;
      t.at = e.timeStamp;
      const match = typeAhead(shape.names, at, t.text);
      e.preventDefault();
      if (match !== null) focusNode(shape.rows[match] ?? i);
      return;
    }
    if (e.shiftKey) return;
    const action = treeKey(e.key, shape.items, at);
    if (!action) return;
    e.preventDefault();
    t.text = "";
    switch (action.kind) {
      case "focus":
        return focusNode(shape.rows[action.index] ?? i);
      case "expand":
      case "collapse":
        return rowToggle(node)?.();
      // Enter does what a double-click does, which a click starts.
      case "activate":
        if (!node.onDoubleClick) return rowToggle(node)?.();
        node.onClick?.();
        return node.onDoubleClick();
      default: {
        const unreachable: never = action;
        return unreachable;
      }
    }
  };

  // The shared menu shows the menu of the row under the pointer. A row with none, or a note, gets no menu.
  const onContextMenu = (e: MouseEvent<HTMLDivElement>) => {
    const i = nodeAt(e.target);
    const node = i === undefined ? undefined : nodes[i];
    if (node?.kind !== "row" || !node.menu) return e.preventDefault();
    setMenu({ key: node.key, content: node.menu() });
  };

  const items = virtualizer.getVirtualItems();
  const measures = virtualizer.measurementsCache;
  const line = (item: VirtualItem, origin: number, inTint: boolean) => {
    const node = nodes[item.index]!;
    return (
      <Line
        key={node.key}
        node={node}
        index={item.index}
        top={item.start - origin}
        inTint={inTint}
        gapAfter={node.tint !== null && shape.groups.get(node.tint)?.last === item.index}
        measure={virtualizer.measureElement}
        onPin={onPin}
        setSize={shape.setSize[item.index] ?? 1}
        posInSet={shape.posInSet[item.index] ?? 1}
        tabStop={item.index === tabStop}
        menuOpen={menu?.key === node.key}
      />
    );
  };
  // A tinted connection's rows sit inside one block that spans the whole connection, rendered rows or not.
  const lines: ReactNode[] = [];
  for (let k = 0; k < items.length;) {
    const tint = nodes[items[k]!.index]!.tint;
    const span = tint && shape.groups.get(tint);
    if (!tint || !span) {
      lines.push(line(items[k]!, 0, false));
      k++;
      continue;
    }
    const run: VirtualItem[] = [];
    while (k < items.length && nodes[items[k]!.index]!.tint === tint) run.push(items[k++]!);
    const top = measures[span.first]?.start ?? 0;
    const bottom = (measures[span.last]?.end ?? top) - GROUP_GAP;
    lines.push(
      <div key={`tint-${tint.name}`} className="absolute inset-x-2 top-0" style={{ transform: `translateY(${top}px)`, height: bottom - top }}>
        <div
          data-testid={`conn-tint-${tint.name}`}
          className="absolute inset-0 rounded-lg"
          style={{ backgroundColor: `color-mix(in srgb, ${tint.hex} 10%, transparent)` }}
        >
          {run.map((item) => line(item, top, true))}
        </div>
      </div>,
    );
  }

  return (
    <ContextMenuArea menu={menu?.content}>
      <div className="group/tree relative" style={{ height: virtualizer.getTotalSize() }} onKeyDown={onKeyDown} onFocus={onFocus} onContextMenu={onContextMenu}>
        {lines}
      </div>
    </ContextMenuArea>
  );
}
