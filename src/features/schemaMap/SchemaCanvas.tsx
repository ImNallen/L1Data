import { KeyRoundIcon, Link2Icon, MaximizeIcon, MinusIcon, PlusIcon, Table2Icon } from "lucide-react";
import { memo, useEffect, useId, useLayoutEffect, useRef, useState, type Dispatch, type KeyboardEvent, type PointerEvent, type SetStateAction } from "react";
import { cx, Highlight, IconButton, Kbd } from "../../ui/primitives";
import type { LinkRoute } from "./layout";
import { visibleColumns, type ColumnRole, type ErColumn, type ErTable, type Focus, type MapColumns, type Oid, type TableKey } from "./model";
import type { SchemaMapView } from "./useSchemaMap";
import { fitViewport, fitWidth, followBox, showRect, zoomViewport, type Point, type Rect, type Size, type Viewport } from "./viewport";

export type CanvasMode = { opens: "fitWidth" | "actualSize"; wheel: "scrollsPage" | "pans" };

const COMPACT = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
export const compactCount = (n: number) => COMPACT.format(n).replace("K", "k");

const SHORT_TYPES: [RegExp, string][] = [
  [/^timestamp(\(\d+\))? with time zone/, "timestamptz$1"],
  [/^timestamp(\(\d+\))? without time zone/, "timestamp$1"],
  [/^time(\(\d+\))? with time zone/, "timetz$1"],
  [/^time(\(\d+\))? without time zone/, "time$1"],
  [/^character varying/, "varchar"],
  [/^character\b/, "char"],
  [/^double precision/, "float8"],
];
export const shortType = (type: string) => SHORT_TYPES.reduce((t, [long, short]) => t.replace(long, short), type);

const DRAG_SLOP = 3;
const ZOOM_STEP = 1.2;

type Tone = "declared" | "inferred";
type Drag = { pointer: number; start: Point; moved: boolean } & ({ kind: "pan"; from: Viewport } | { kind: "card"; key: TableKey; from: Point; scale: number });

function needleRanges(text: string, needle: string) {
  const at = needle ? text.toLowerCase().indexOf(needle) : -1;
  return at < 0 ? null : [{ start: at, end: at + needle.length }];
}

export const ROLE_ICON: Partial<Record<ColumnRole, { icon: typeof KeyRoundIcon; className: string; label: string }>> = {
  pk: { icon: KeyRoundIcon, className: "text-amber", label: "Primary key" },
  pkfk: { icon: KeyRoundIcon, className: "text-amber", label: "Primary key and foreign key" },
  fk: { icon: Link2Icon, className: "text-keyword", label: "Foreign key" },
  inferred: { icon: Link2Icon, className: "text-amber", label: "Inferred reference" },
};

const ColumnRow = memo(function ColumnRow({ column, tone, needle, height }: { column: ErColumn; tone: Tone | undefined; needle: string; height: number }) {
  const role = ROLE_ICON[column.role];
  const Icon = role?.icon;
  return (
    <div
      data-column={column.name}
      data-tone={tone}
      className={cx(
        "flex items-center gap-1.5 px-2.5 font-mono text-[11.5px]",
        tone === "declared" && "bg-keyword/12",
        tone === "inferred" && "bg-amber/12",
        column.role === "plain" ? "text-muted" : "text-fg",
      )}
      style={{ height }}
    >
      <span className="grid w-3 shrink-0 place-items-center">{Icon && <Icon size={11} className={role.className} aria-label={role.label} />}</span>
      <span className="min-w-0 flex-1 truncate">
        <Highlight text={column.name} ranges={needleRanges(column.name, needle)} />
      </span>
      <span className="shrink-0 text-[10.5px] text-dim">{shortType(column.type)}</span>
    </div>
  );
});

const TableCard = memo(function TableCard({
  table,
  rect,
  columns,
  header,
  row,
  lit,
  focused,
  tones,
  needle,
}: {
  table: ErTable;
  rect: Rect;
  columns: MapColumns;
  header: number;
  row: number;
  lit: boolean;
  focused: boolean;
  tones: ReadonlyMap<string, Tone> | undefined;
  needle: string;
}) {
  const name = table.external ? `${table.schema}.${table.name}` : table.name;
  return (
    <div
      data-map-card={table.key}
      data-testid={`map-table-${table.key}`}
      data-focused={focused}
      data-lit={lit}
      className={cx(
        "absolute cursor-default overflow-hidden rounded-md border bg-raised shadow-[var(--shadow-float)] transition-opacity select-none",
        focused ? "border-focus ring-1 ring-focus/40" : "border-control",
        table.external && "border-dashed",
        !lit && "opacity-30",
      )}
      style={{ left: rect.x, top: rect.y, width: rect.width, height: rect.height }}
    >
      <div className={cx("flex items-center gap-1.5 border-b border-control px-2.5", focused && "bg-keyword/10")} style={{ height: header }}>
        <Table2Icon size={12} className={cx("shrink-0", focused ? "text-keyword" : "text-dim")} />
        <span className="min-w-0 flex-1 truncate font-mono text-[12.5px] font-semibold text-fg" title={`${table.schema}.${table.name}`}>
          <Highlight text={name} ranges={needleRanges(name, needle)} />
        </span>
        {table.rowEstimate !== null && <span className="shrink-0 font-mono text-[10px] text-dim">{compactCount(table.rowEstimate)}</span>}
      </div>
      {visibleColumns(table, columns).map((c) => (
        <ColumnRow key={c.attnum} column={c} tone={tones?.get(c.name)} needle={needle} height={row} />
      ))}
    </div>
  );
});

function LinkPath({ route, tone, lit, markers }: { route: LinkRoute; tone: "base" | "focus" | "inferred"; lit: boolean; markers: string }) {
  const stroke = { base: "var(--color-faint)", focus: "var(--color-keyword)", inferred: "var(--color-amber)" }[tone];
  return (
    <g opacity={lit ? 1 : 0.15} data-testid={`map-link-${route.id}`} data-lit={lit}>
      <path
        d={route.path}
        fill="none"
        stroke={stroke}
        strokeWidth={tone === "focus" ? 1.6 : 1.2}
        strokeDasharray={route.dashed ? "4 3" : undefined}
        markerStart={`url(#${markers}-${route.source.marker}-${tone})`}
        markerEnd={`url(#${markers}-${route.target.marker}-${tone})`}
      />
    </g>
  );
}

function Markers({ id }: { id: string }) {
  const tones = { base: "var(--color-faint)", focus: "var(--color-keyword)", inferred: "var(--color-amber)" };
  return (
    <defs>
      {Object.entries(tones).map(([tone, color]) => [
        <marker
          key={`many-${tone}`}
          id={`${id}-many-${tone}`}
          viewBox="-16 -7 18 14"
          refX={0}
          refY={0}
          markerWidth={18}
          markerHeight={14}
          markerUnits="userSpaceOnUse"
          orient="auto-start-reverse"
        >
          <path d="M -10 0 L 0 -5 M -10 0 L 0 0 M -10 0 L 0 5 M -14 -5 L -14 5" fill="none" stroke={color} strokeWidth={1.2} />
        </marker>,
        <marker
          key={`one-${tone}`}
          id={`${id}-one-${tone}`}
          viewBox="-16 -7 18 14"
          refX={0}
          refY={0}
          markerWidth={18}
          markerHeight={14}
          markerUnits="userSpaceOnUse"
          orient="auto-start-reverse"
        >
          <path d="M -7 -5 L -7 5" fill="none" stroke={color} strokeWidth={1.2} />
        </marker>,
      ])}
    </defs>
  );
}

type SetViewport = Dispatch<SetStateAction<Viewport>>;

function useFollowBox(focus: Focus | null, focusRect: Rect | undefined, box: Size, setViewport: SetViewport) {
  const [shownWidth, setShownWidth] = useState(0);
  const [revealed, setRevealed] = useState<string | null>(null);
  const resized = shownWidth > 0 && shownWidth !== box.width;
  if (shownWidth !== box.width) setShownWidth(box.width);
  const revealKey = focus ? `${focus.table.key}\u0000${box.width}\u0000${box.height}` : null;
  const reveals = revealed !== revealKey && box.width > 0 && (!focus || focusRect);
  if (reveals) setRevealed(revealKey);
  if (resized || (reveals && focusRect)) setViewport((v) => followBox(v, resized ? shownWidth : box.width, box, reveals ? focusRect : undefined));
}

export function SchemaCanvas({
  map,
  mode,
  height,
  testId,
  label,
  gestureHint,
}: {
  map: SchemaMapView;
  mode: CanvasMode;
  height: number | "fill";
  testId: string;
  label: string;
  gestureHint?: string;
}) {
  const { model, layout, routes, focus, matches, view, columns } = map;
  const canvas = useRef<HTMLDivElement>(null);
  const drag = useRef<Drag | null>(null);
  const markers = useId().replace(/[^\w-]/g, "");
  const [box, setBox] = useState<Size>({ width: 0, height: 0 });
  const [viewport, setViewport] = useState<Viewport>({ x: 0, y: 0, scale: 1 });
  const [fitted, setFitted] = useState<string | null>(null);
  // Chromium matches :focus-visible on any key pressed after a click, so Esc after clicking a card would ring the whole canvas.
  const [keyboardFocus, setKeyboardFocus] = useState(false);

  useLayoutEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const measure = () => setBox((b) => (b.width === el.clientWidth && b.height === el.clientHeight ? b : { width: el.clientWidth, height: el.clientHeight }));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const fitKey = model ? `${model.version}\u0000${model.schema}\u0000${columns}` : null;
  const focusRect = focus ? layout?.cards.get(focus.table.oid) : undefined;
  if (layout && fitKey && box.width > 0 && fitted !== fitKey) {
    setFitted(fitKey);
    const fits = fitViewport(layout.bounds, box, { maxScale: 1 }).scale === 1;
    setViewport(
      mode.opens === "fitWidth" ? fitWidth(layout.bounds, box, { maxScale: 1 }) : showRect(fits ? layout.bounds : (focusRect ?? layout.bounds), box, 1),
    );
  }
  useFollowBox(focus, focusRect, box, setViewport);

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    // React's wheel listener is passive, and a zoom must keep the notebook from scrolling.
    const onWheel = (e: WheelEvent) => {
      const zooms = e.ctrlKey || e.metaKey;
      if (!zooms && mode.wheel === "scrollsPage") return;
      e.preventDefault();
      const r = el.getBoundingClientRect();
      if (zooms) setViewport((v) => zoomViewport(v, Math.exp(Math.max(-0.2, Math.min(0.2, -e.deltaY / 100))), { x: e.clientX - r.left, y: e.clientY - r.top }));
      else setViewport((v) => ({ ...v, x: v.x - e.deltaX, y: v.y - e.deltaY }));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [mode.wheel]);

  const zoom = (factor: number) => setViewport((v) => zoomViewport(v, factor, { x: box.width / 2, y: box.height / 2 }));
  const fitAll = () => layout && box.width > 0 && setViewport(fitViewport(layout.bounds, box, { maxScale: 1 }));

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    setKeyboardFocus(false);
    if (e.button !== 0 || !layout || !model) return;
    const key = (e.target as Element).closest<HTMLElement>("[data-map-card]")?.dataset.mapCard as TableKey | undefined;
    const oid = key ? model.byKey.get(key) : undefined;
    const rect = oid !== undefined ? layout.cards.get(oid) : undefined;
    const start = { x: e.clientX, y: e.clientY };
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current =
      key && rect
        ? { kind: "card", key, from: { x: rect.x, y: rect.y }, scale: viewport.scale, pointer: e.pointerId, start, moved: false }
        : { kind: "pan", from: viewport, pointer: e.pointerId, start, moved: false };
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const held = drag.current;
    if (!held || held.pointer !== e.pointerId) return;
    const dx = e.clientX - held.start.x;
    const dy = e.clientY - held.start.y;
    if (!held.moved && Math.hypot(dx, dy) < DRAG_SLOP) return;
    held.moved = true;
    if (held.kind === "pan") setViewport({ ...held.from, x: held.from.x + dx, y: held.from.y + dy });
    else map.actions.move(held.key, { x: Math.round(held.from.x + dx / held.scale), y: Math.round(held.from.y + dy / held.scale) });
  };
  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    const held = drag.current;
    if (!held || held.pointer !== e.pointerId) return;
    drag.current = null;
    if (held.moved) return;
    if (held.kind === "card") map.actions.focus(view.focus === held.key ? null : held.key);
    else map.actions.focus(null);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && view.focus) {
      e.preventDefault();
      e.stopPropagation();
      map.actions.focus(null);
    } else if (e.target === e.currentTarget && (e.key === "+" || e.key === "=" || e.key === "-")) {
      e.preventDefault();
      zoom(e.key === "-" ? 1 / ZOOM_STEP : ZOOM_STEP);
    }
  };

  const tones = new Map<Oid, Map<string, Tone>>();
  for (const link of focus?.links ?? []) {
    const tone: Tone = link.basis === "Declared" ? "declared" : "inferred";
    for (const pair of link.pairs) {
      for (const [oid, column] of [
        [link.source, pair.source],
        [link.target, pair.target],
      ] as const) {
        const held = tones.get(oid) ?? new Map<string, Tone>();
        tones.set(oid, held.set(column, held.get(column) === "declared" ? "declared" : tone));
      }
    }
  }
  const litTable = (oid: Oid) => (focus ? focus.related.has(oid) : matches ? matches.has(oid) : true);
  const needle = view.find.trim().toLowerCase();
  const m = layout?.metrics;

  return (
    <div className="relative h-full" style={height === "fill" ? undefined : { height }}>
      <div
        ref={canvas}
        data-testid={testId}
        tabIndex={0}
        role="region"
        aria-label={label}
        data-keyboard-focus={keyboardFocus || undefined}
        className="map-canvas absolute inset-0 touch-none overflow-hidden outline-none data-[keyboard-focus]:ring-1 data-[keyboard-focus]:ring-focus data-[keyboard-focus]:ring-inset"
        onFocus={(e) => e.target === e.currentTarget && setKeyboardFocus(e.currentTarget.matches(":focus-visible"))}
        onBlur={(e) => e.target === e.currentTarget && setKeyboardFocus(false)}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={() => {
          drag.current = null;
        }}
        onKeyDown={onKeyDown}
      >
        {model && layout && m && (
          <div className="absolute top-0 left-0 origin-top-left" style={{ transform: `translate(${viewport.x}px, ${viewport.y}px) scale(${viewport.scale})` }}>
            {layout.unlinkedFrame && (
              <div
                className={cx("absolute rounded-lg border border-dashed border-control transition-opacity", (focus || matches) && "opacity-40")}
                style={{ left: layout.unlinkedFrame.x, top: layout.unlinkedFrame.y, width: layout.unlinkedFrame.width, height: layout.unlinkedFrame.height }}
              >
                <span className="absolute left-3 text-[11px] text-dim" style={{ lineHeight: `${m.header}px` }}>
                  No relationships · {model.isolated.length}
                </span>
              </div>
            )}
            <svg width="1" height="1" className="absolute overflow-visible" aria-hidden>
              <Markers id={markers} />
              {routes.map((route) => {
                const link = model.links.get(route.id)!;
                const touches = focus ? link.source === focus.table.oid || link.target === focus.table.oid : null;
                const lit = touches ?? (matches ? matches.has(link.source) || matches.has(link.target) : true);
                const tone = route.dashed ? "inferred" : touches ? "focus" : "base";
                return <LinkPath key={route.id} route={route} tone={tone} lit={lit} markers={markers} />;
              })}
            </svg>
            {[...model.tables.values()].map((table) => {
              const rect = layout.cards.get(table.oid);
              return rect ? (
                <TableCard
                  key={table.oid}
                  table={table}
                  rect={rect}
                  columns={columns}
                  header={m.header}
                  row={m.row}
                  lit={litTable(table.oid)}
                  focused={focus?.table.oid === table.oid}
                  tones={tones.get(table.oid)}
                  needle={needle}
                />
              ) : null;
            })}
          </div>
        )}
      </div>
      <div className="pointer-events-none absolute bottom-3 left-3 flex items-center gap-4">
        {gestureHint && <span className="text-ui-11.5 whitespace-nowrap text-dim">{gestureHint}</span>}
        {focus && (
          <div
            data-testid="map-focus-pill"
            className="pointer-events-auto flex h-8 items-center gap-2 rounded-full border border-control bg-raised pr-1 pl-3 text-ui-12 text-muted shadow-[var(--shadow-float)]"
          >
            <span>
              Focused on <span className="font-mono font-semibold text-fg">{focus.table.name}</span>
            </span>
            <span className="text-dim">
              · {focus.counts.references + focus.counts.referencedBy} keys · {focus.counts.possible} inferred
            </span>
            <button
              type="button"
              className="inline-flex h-6 items-center gap-1 rounded-full bg-pill px-2 text-ui-11.5 text-muted hover:bg-hover hover:text-fg"
              onClick={() => map.actions.focus(null)}
            >
              <Kbd tone="menu" className="h-4 px-1">
                Esc
              </Kbd>
              Clear
            </button>
          </div>
        )}
      </div>
      {model && layout && (
        <div className="absolute right-3 bottom-3 flex items-center gap-0.5 rounded-lg border border-control bg-raised p-1 shadow-[var(--shadow-float)]">
          <IconButton title="Zoom out" aria-label="Zoom out" onClick={() => zoom(1 / ZOOM_STEP)}>
            <MinusIcon size={14} />
          </IconButton>
          <span data-testid="map-zoom" className="w-11 text-center text-ui-11.5 text-muted tabular-nums">
            {Math.round(viewport.scale * 100)}%
          </span>
          <IconButton title="Zoom in" aria-label="Zoom in" onClick={() => zoom(ZOOM_STEP)}>
            <PlusIcon size={14} />
          </IconButton>
          <span aria-hidden className="mx-0.5 h-4 w-px bg-control" />
          <IconButton title="Fit map" aria-label="Fit map" onClick={() => void fitAll()}>
            <MaximizeIcon size={14} />
          </IconButton>
        </div>
      )}
    </div>
  );
}
