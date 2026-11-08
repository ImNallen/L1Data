import * as Menu from "@radix-ui/react-dropdown-menu";
import { ChevronDownIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { commands, type ChartSeries } from "../../ipc/bindings";
import type { RowResult } from "../../state/status";
import { cx, MenuContent, MenuItem } from "../../ui/primitives";
import { niceTicks, yColumns, type ChartKind, type ChartSpec } from "./chart";
import { groupDigits } from "./format";

const MAX_POINTS = 2_000;
const STREAM_REFETCH_MS = 300;
const KINDS: ChartKind[] = ["Bar", "Line"];
const LABEL_CHARS = 14;
const CHAR_PX = 6.5;
const MARGIN = { top: 12, right: 16, bottom: 24, left: 8 };

const compact = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 2 });
const exact = new Intl.NumberFormat(undefined, { maximumFractionDigits: 6 });

function Chip({ label, value, testId, children }: { label: string; value: string; testId: string; children: ReactNode }) {
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger
        data-testid={testId}
        className="inline-flex h-6 max-w-56 items-center gap-1 rounded-md border border-control pr-1 pl-2 text-ui-12 whitespace-nowrap hover:bg-hover data-[state=open]:bg-hover"
      >
        <span className="text-dim">{label}</span>
        <span className="truncate text-fg">{value}</span>
        <ChevronDownIcon size={12} className="shrink-0 text-muted" />
      </Menu.Trigger>
      <MenuContent>{children}</MenuContent>
    </Menu.Root>
  );
}

function short(label: string | null): string {
  const text = label ?? "NULL";
  return text.length > LABEL_CHARS ? `${text.slice(0, LABEL_CHARS - 1)}…` : text;
}

/** A bar with its data end rounded, which is the top for a positive value and the bottom for a negative one. */
function barPath(x: number, w: number, base: number, end: number): string {
  const h = Math.abs(end - base);
  const r = Math.min(4, w / 2, h);
  const dir = end < base ? 1 : -1;
  const tip = end + dir * r;
  return [`M${x},${base}`, `V${tip}`, `Q${x},${end} ${x + r},${end}`, `H${x + w - r}`, `Q${x + w},${end} ${x + w},${tip}`, `V${base}`, "Z"].join("");
}

function linePath(values: (number | null)[], xAt: (i: number) => number, yAt: (v: number) => number): string {
  let d = "";
  let open = false;
  values.forEach((v, i) => {
    if (v === null) {
      open = false;
      return;
    }
    d += `${open ? "L" : "M"}${xAt(i)},${yAt(v)}`;
    // A lone point between gaps is a zero-length segment, which the round cap draws as a dot.
    if (!open && (i + 1 >= values.length || values[i + 1] === null)) d += `L${xAt(i)},${yAt(v)}`;
    open = true;
  });
  return d;
}

function Plot({ series, kind, xName, yName }: { series: ChartSeries; kind: ChartKind; xName: string; yName: string }) {
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const [active, setActive] = useState<number | null>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setSize({ width: entry.contentRect.width, height: entry.contentRect.height });
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const n = series.values.length;
  const numbers = series.values.filter((v): v is number => v !== null);
  let lo = numbers.length ? Math.min(...numbers) : 0;
  let hi = numbers.length ? Math.max(...numbers) : 1;
  if (kind === "Bar") {
    lo = Math.min(lo, 0);
    hi = Math.max(hi, 0);
  }
  const ticks = niceTicks(lo, hi);
  const domainLo = ticks[0] ?? lo;
  const domainHi = ticks.at(-1) ?? hi;
  const tickText = ticks.map((t) => compact.format(t));
  const left = MARGIN.left + Math.max(...tickText.map((t) => t.length)) * CHAR_PX + 8;
  const plotW = Math.max(0, size.width - left - MARGIN.right);
  const plotH = Math.max(0, size.height - MARGIN.top - MARGIN.bottom);
  const band = n ? plotW / n : 0;
  const xAt = (i: number) => left + (i + 0.5) * band;
  const yAt = (v: number) => MARGIN.top + plotH - ((v - domainLo) / (domainHi - domainLo || 1)) * plotH;
  const base = yAt(Math.min(Math.max(0, domainLo), domainHi));
  const barW = Math.max(1, Math.min(40, band * 0.6, band - 2));
  const labelW = Math.max(...series.labels.map((l) => short(l).length)) * CHAR_PX + 12;
  const labelStep = Math.max(1, Math.ceil(labelW / (band || 1)));
  const labelX = (i: number, text: string) => {
    const half = (text.length * CHAR_PX) / 2;
    return Math.min(Math.max(xAt(i), half), size.width - half);
  };
  const shown = active !== null && active < n ? active : null;

  const pick = (e: PointerEvent<SVGSVGElement>) => {
    const x = e.clientX - e.currentTarget.getBoundingClientRect().left;
    if (band <= 0 || x < left || x > left + plotW) return setActive(null);
    setActive(Math.min(n - 1, Math.floor((x - left) / band)));
  };
  const step = (e: KeyboardEvent<SVGSVGElement>) => {
    const move = { ArrowLeft: -1, ArrowRight: 1, Home: -n, End: n }[e.key];
    if (move === undefined || n === 0) return;
    e.preventDefault();
    setActive((at) => Math.max(0, Math.min(n - 1, (at ?? (move > 0 ? -1 : n)) + move)));
  };

  const value = shown === null ? undefined : series.values[shown];
  return (
    <div ref={box} className="relative min-h-0 flex-1">
      <svg
        role="img"
        aria-label={`${kind} chart of ${yName} by ${xName}, ${n} ${n === 1 ? "point" : "points"}`}
        data-testid="chart-plot"
        tabIndex={0}
        width={size.width}
        height={size.height}
        className="absolute inset-0 outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-focus"
        onPointerMove={pick}
        onPointerLeave={() => setActive(null)}
        onKeyDown={step}
        onBlur={() => setActive(null)}
      >
        {plotW > 0 && plotH > 0 && (
          <>
            {ticks.map((t, i) => (
              <g key={t}>
                <line x1={left} x2={left + plotW} y1={yAt(t)} y2={yAt(t)} stroke="var(--color-border)" />
                <text x={left - 8} y={yAt(t)} dy="0.32em" textAnchor="end" className="fill-dim text-ui-11 tabular-nums">
                  {tickText[i]}
                </text>
              </g>
            ))}
            {kind === "Bar" && shown !== null && <rect x={left + shown * band} y={MARGIN.top} width={band} height={plotH} fill="var(--color-hover)" />}
            {kind === "Bar" &&
              series.values.map((v, i) =>
                v === null || v === 0 ? null : <path key={i} d={barPath(xAt(i) - barW / 2, barW, base, yAt(v))} fill="var(--color-run)" />,
              )}
            <line x1={left} x2={left + plotW} y1={base} y2={base} stroke="var(--color-control)" />
            {kind === "Line" && (
              <path d={linePath(series.values, xAt, yAt)} fill="none" stroke="var(--color-run)" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
            )}
            {kind === "Line" && shown !== null && (
              <>
                <line x1={xAt(shown)} x2={xAt(shown)} y1={MARGIN.top} y2={MARGIN.top + plotH} stroke="var(--color-faint)" />
                {value != null && <circle cx={xAt(shown)} cy={yAt(value)} r={4} fill="var(--color-run)" stroke="var(--color-bg)" strokeWidth={2} />}
              </>
            )}
            {series.labels.map((l, i) =>
              i % labelStep === 0 ? (
                <text key={i} x={labelX(i, short(l))} y={MARGIN.top + plotH + 16} textAnchor="middle" className="fill-dim text-ui-11">
                  {short(l)}
                </text>
              ) : null,
            )}
          </>
        )}
      </svg>
      {shown !== null && (
        <div
          data-testid="chart-tooltip"
          role="status"
          className="pointer-events-none absolute top-2 z-10 max-w-64 rounded-md border border-control bg-raised px-2.5 py-1.5 text-ui-12 shadow-[var(--shadow-float)]"
          style={xAt(shown) > size.width / 2 ? { right: size.width - xAt(shown) + 12 } : { left: xAt(shown) + 12 }}
        >
          <div className="font-medium text-fg tabular-nums">{value == null ? "No value" : exact.format(value)}</div>
          <div className="truncate text-muted">{yName}</div>
          <div className="truncate text-dim">{series.labels[shown] ?? "NULL"}</div>
        </div>
      )}
    </div>
  );
}

export function ChartView({ result, spec, onSpec }: { result: RowResult; spec: ChartSpec; onSpec: (spec: ChartSpec) => void }) {
  const { meta } = result;
  const columns = meta.columns;
  const key = `${meta.ref.tabId}:${meta.ref.runId}:${meta.ref.index}:${spec.x}:${spec.y}`;
  const [loaded, setLoaded] = useState<{ key: string; series: ChartSeries } | null>(null);
  const issued = useRef(0);
  const lastAt = useRef(-Infinity);
  const streaming = result.state === "Streaming";

  useEffect(() => {
    const wait = streaming ? Math.max(0, lastAt.current + STREAM_REFETCH_MS - performance.now()) : 0;
    const timer = setTimeout(() => {
      lastAt.current = performance.now();
      const ask = ++issued.current;
      void commands.resultSeries(meta.ref, spec.x, spec.y, MAX_POINTS).then((res) => {
        if (ask === issued.current && res.status === "ok") setLoaded({ key, series: res.data });
      });
    }, wait);
    return () => clearTimeout(timer);
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the key names the ref's fields; each console snapshot brings a new ref object
  }, [key, meta.rowCount, streaming]);

  const series = loaded?.series;
  const xName = spec.x === null ? "row number" : (columns[spec.x]?.name ?? "");
  const yName = columns[spec.y]?.name ?? "";
  const count = series?.labels.length ?? 0;
  const note = !series
    ? ""
    : series.capped
      ? `First ${groupDigits(String(count))} of ${groupDigits(String(meta.rowCount))} stored rows`
      : `All ${groupDigits(String(count))} stored ${count === 1 ? "row" : "rows"}`;

  return (
    <div data-testid="chart-view" className="flex min-h-0 flex-1 flex-col bg-bg">
      <div className="flex shrink-0 flex-wrap items-center gap-1.5 px-3 pt-2.5 pb-1">
        <Chip label="Type" value={spec.kind} testId="chart-kind">
          {KINDS.map((k) => (
            <MenuItem key={k} testId={`chart-kind-${k}`} checked={k === spec.kind} onSelect={() => onSpec({ ...spec, kind: k })}>
              {k}
            </MenuItem>
          ))}
        </Chip>
        <Chip label="X" value={spec.x === null ? "Row number" : (columns[spec.x]?.name ?? "")} testId="chart-x">
          <MenuItem testId="chart-x-row" checked={spec.x === null} onSelect={() => onSpec({ ...spec, x: null })}>
            Row number
          </MenuItem>
          {columns.map((c, i) => (
            <MenuItem key={i} testId={`chart-x-${i}`} checked={spec.x === i} onSelect={() => onSpec({ ...spec, x: i })}>
              {c.name}
            </MenuItem>
          ))}
        </Chip>
        <Chip label="Y" value={yName} testId="chart-y">
          {yColumns(columns).map((i) => (
            <MenuItem key={i} testId={`chart-y-${i}`} checked={spec.y === i} onSelect={() => onSpec({ ...spec, y: i })}>
              {columns[i]?.name}
            </MenuItem>
          ))}
        </Chip>
        <span data-testid="chart-note" className="ml-auto text-ui-12 text-dim tabular-nums">
          {note}
        </span>
      </div>
      {series && count === 0 ? (
        <div className="grid flex-1 place-items-center bg-bg text-ui-13 text-dim">No rows to chart.</div>
      ) : (
        <div className={cx("flex min-h-0 flex-1 flex-col pb-2", loaded?.key !== key && "opacity-60")}>
          {series && <Plot series={series} kind={spec.kind} xName={xName} yName={yName} />}
        </div>
      )}
    </div>
  );
}
