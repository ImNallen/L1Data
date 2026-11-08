import * as Popover from "@radix-ui/react-popover";
import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { commands, type ColumnMeta, type ColumnProfile, type HistogramBin, type ResultMeta, type ValueCount } from "../../ipc/bindings";
import { cx } from "../../ui/primitives";
import { EMPTY_MARK } from "./GridRows";
import { groupDigits, isNumeric } from "./format";

const PLOT_W = 276;
const PLOT_H = 56;
const BAR_GAP = 2;

const exact = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 15 });
/** Two digits finer than the bin width: enough to tell edges apart, without float noise like 209.996666666667. */
function edgeText(lo: number, hi: number): string {
  if (lo === hi) return exact.format(lo);
  const digits = Math.min(20, Math.max(0, Math.ceil(-Math.log10(hi - lo)) + 2));
  const format = new Intl.NumberFormat(undefined, { maximumFractionDigits: digits });
  return `${format.format(lo)} – ${format.format(hi)}`;
}
const count = (n: number) => groupDigits(String(n));
const rows = (n: number) => `${count(n)} ${n === 1 ? "row" : "rows"}`;

/** Where the rows came from: every stored row of a result, or one page of a table tab. */
export type ProfileScope = "result" | "page";

function footnote(profile: ColumnProfile, meta: ResultMeta, scope: ProfileScope): string {
  if (scope === "page") return `From the ${rows(profile.rows)} on this page.`;
  if (meta.truncated !== null) {
    const why = meta.truncated === "RowLimit" ? "row limit" : "size limit";
    return `From the ${count(profile.rows)} stored rows of ${count(meta.rowsSeen)} read (${why}).`;
  }
  return profile.rows === 1 ? "From the 1 stored row." : `From all ${count(profile.rows)} stored rows.`;
}

function Stat({ label, value }: { label: string; value: string | null }) {
  return (
    <>
      <dt className="text-muted">{label}</dt>
      <dd title={value ?? undefined} className={cx("truncate text-right font-mono tabular-nums", value === null ? "text-faint" : "text-fg")}>
        {value ?? "–"}
      </dd>
    </>
  );
}

function barPath(x: number, w: number, top: number): string {
  const r = Math.min(2, w / 2, PLOT_H - top);
  return `M${x},${PLOT_H}V${top + r}Q${x},${top} ${x + r},${top}H${x + w - r}Q${x + w},${top} ${x + w},${top + r}V${PLOT_H}Z`;
}

function Histogram({ bins }: { bins: HistogramBin[] }) {
  const [active, setActive] = useState<number | null>(null);
  const most = Math.max(1, ...bins.map((b) => b.count));
  const barW = (PLOT_W - BAR_GAP * (bins.length - 1)) / bins.length;
  const shown = active === null ? undefined : bins[active];
  const lo = bins[0]?.lo ?? 0;
  const hi = bins.at(-1)?.hi ?? 0;
  return (
    <div className="mt-3">
      <svg role="img" aria-label={`Histogram of ${bins.length} ${bins.length === 1 ? "bin" : "bins"}`} width={PLOT_W} height={PLOT_H + 1} className="block">
        {bins.map((b, i) => {
          const x = i * (barW + BAR_GAP);
          const top = PLOT_H - (b.count / most) * PLOT_H;
          return (
            <g key={i} data-testid={`profile-bin-${i}`} onMouseEnter={() => setActive(i)} onMouseLeave={() => setActive(null)}>
              <rect x={x} y={0} width={barW} height={PLOT_H} fill={active === i ? "var(--color-hover)" : "transparent"} />
              {b.count > 0 && <path d={barPath(x, barW, top)} fill="var(--color-run)" />}
            </g>
          );
        })}
        <line x1={0} x2={PLOT_W} y1={PLOT_H + 0.5} y2={PLOT_H + 0.5} stroke="var(--color-control)" />
      </svg>
      {shown ? (
        <div role="status" data-testid="profile-bin-tooltip" className="mt-1 truncate text-center text-ui-11.5 text-fg tabular-nums">
          {edgeText(shown.lo ?? 0, shown.hi ?? 0)} · {rows(shown.count)}
        </div>
      ) : (
        <div className="mt-1 flex justify-between font-mono text-ui-11 text-dim tabular-nums">
          <span>{exact.format(lo)}</span>
          <span>{exact.format(hi)}</span>
        </div>
      )}
    </div>
  );
}

function TopValues({ top, nonNull }: { top: ValueCount[]; nonNull: number }) {
  if (top.length === 0) return <p className="mt-3 text-ui-12 text-dim">Every value is NULL.</p>;
  return (
    <ul aria-label="Most frequent values" className="mt-3 flex flex-col gap-1.5">
      {top.map((v, i) => (
        <li key={i} className="text-ui-12">
          <div className="flex items-baseline gap-3">
            {v.value === "" ? (
              <span className="text-ui-11 text-dim italic">{EMPTY_MARK}</span>
            ) : (
              <span title={v.value} className="min-w-0 flex-1 truncate font-mono text-fg">
                {v.value}
              </span>
            )}
            <span className="ml-auto shrink-0 font-mono text-muted tabular-nums">{count(v.count)}</span>
          </div>
          <div className="mt-0.5 h-[3px] rounded-full bg-run/25" style={{ width: `${(v.count / Math.max(1, nonNull)) * 100}%` }} />
        </li>
      ))}
    </ul>
  );
}

function Placeholder({ numeric }: { numeric: boolean }) {
  return numeric ? (
    <div className="mt-3">
      <div className="rounded-md bg-hover" style={{ height: PLOT_H + 1 }} />
      <div className="mt-1 h-4" />
    </div>
  ) : (
    <div className="mt-3 flex flex-col gap-1.5">
      {[0, 1, 2, 3, 4].map((i) => (
        <div key={i} className="h-[23px] rounded-md bg-hover" />
      ))}
    </div>
  );
}

/** A column that may be summarized as numbers, so the loading view matches the numeric layout and the popover keeps its size. */
const looksNumeric = (column: ColumnMeta) => isNumeric(column.type.category) && column.type.display !== "money" && column.type.display !== "smallmoney";

type Loaded = { kind: "loading" } | { kind: "ready"; profile: ColumnProfile } | { kind: "failed" };

function useProfile(meta: ResultMeta, col: number, done: boolean, onGone: () => void): Loaded {
  const [loaded, setLoaded] = useState<Loaded>({ kind: "loading" });
  const issued = useRef(0);
  const goneRef = useRef(onGone);
  goneRef.current = onGone;
  useEffect(() => {
    const ask = ++issued.current;
    void commands.resultProfile(meta.ref, col).then((res) => {
      if (ask !== issued.current) return;
      if (res.status === "ok") setLoaded({ kind: "ready", profile: res.data });
      else if (res.error.kind === "Gone") goneRef.current();
      else setLoaded({ kind: "failed" });
    });
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- a result's ref never changes while the grid that owns it is mounted
  }, [col, done]);
  return loaded;
}

function ProfileBody({ meta, col, scope, done, onClose }: { meta: ResultMeta; col: number; scope: ProfileScope; done: boolean; onClose: () => void }) {
  const column = meta.columns[col]!;
  const state = useProfile(meta, col, done, onClose);
  const profile = state.kind === "ready" ? state.profile : null;
  const summary = profile?.summary;
  const numeric = summary ? summary.kind === "Numeric" : looksNumeric(column);
  const stat = (n: number | undefined) => (n === undefined ? null : count(n));
  return (
    <>
      <div className="flex min-w-0 items-baseline gap-2">
        <span title={column.name} className="min-w-0 truncate font-mono text-ui-13 font-medium text-fg">
          {column.name}
        </span>
        <span title={column.type.display} className="ml-auto shrink-0 truncate font-mono text-ui-11.5 text-dim">
          {column.type.display}
        </span>
      </div>
      <dl
        className={cx(
          "mt-3 grid gap-x-3 gap-y-1 text-ui-12",
          numeric ? "grid-cols-[auto_auto_auto_minmax(0,1fr)] [&>dt:nth-child(4n+3)]:pl-2" : "grid-cols-[auto_minmax(0,1fr)]",
        )}
      >
        {numeric ? (
          <>
            <Stat label="Rows" value={stat(profile?.rows)} />
            <Stat label="Min" value={summary?.kind === "Numeric" ? groupDigits(summary.min) : null} />
            <Stat label="Nulls" value={stat(profile?.nulls)} />
            <Stat label="Mean" value={summary?.kind === "Numeric" ? groupDigits(summary.mean) : null} />
            <Stat label="Distinct" value={stat(profile?.distinct)} />
            <Stat label="Max" value={summary?.kind === "Numeric" ? groupDigits(summary.max) : null} />
          </>
        ) : (
          <>
            <Stat label="Rows" value={stat(profile?.rows)} />
            <Stat label="Nulls" value={stat(profile?.nulls)} />
            <Stat label="Distinct" value={stat(profile?.distinct)} />
          </>
        )}
      </dl>
      {state.kind === "failed" ? (
        <p className="mt-3 text-ui-12 text-danger">Could not profile this column.</p>
      ) : !profile || !summary ? (
        <Placeholder numeric={numeric} />
      ) : summary.kind === "Numeric" ? (
        summary.histogram.length > 0 && <Histogram bins={summary.histogram} />
      ) : (
        <TopValues top={summary.top} nonNull={profile.rows - profile.nulls} />
      )}
      <p role="status" className="mt-3 text-ui-11.5 text-dim">
        {profile ? footnote(profile, meta, scope) : "Profiling…"}
      </p>
    </>
  );
}

const headerIn = (grid: HTMLElement | null, col: number) => grid?.querySelector<HTMLElement>(`[data-header="${col}"]`) ?? null;

/**
 * The profile of column `col`, under its header in `grid`, which marks each header with `data-header`. Closing hands
 * focus back to the grid.
 */
export function ColumnProfilePopover({
  meta,
  col,
  scope,
  done,
  grid,
  onClose,
}: {
  meta: ResultMeta;
  col: number | null;
  scope: ProfileScope;
  done: boolean;
  grid: RefObject<HTMLElement | null>;
  onClose: () => void;
}) {
  // The header can scroll away or unmount while the popover is open, so the anchor keeps its last box.
  const last = useRef<DOMRect | null>(null);
  const anchor = useMemo(
    () => ({
      current: {
        getBoundingClientRect() {
          const el = col === null ? null : headerIn(grid.current, col);
          if (el) last.current = el.getBoundingClientRect();
          return last.current ?? new DOMRect();
        },
        get contextElement() {
          return grid.current ?? undefined;
        },
      },
    }),
    [grid, col],
  );
  return (
    <Popover.Root open={col !== null} onOpenChange={(open) => !open && onClose()}>
      <Popover.Anchor virtualRef={anchor} />
      <Popover.Portal>
        {col !== null && (
          <Popover.Content
            data-testid="column-profile"
            aria-label={`Profile of ${meta.columns[col]?.name ?? ""}`}
            side="bottom"
            align="start"
            sideOffset={4}
            collisionPadding={8}
            // A press on a header opens, switches, or closes the profile with the header's own click, and focus
            // moving back to the grid is no reason to close.
            onPointerDownOutside={(e) => {
              if (e.target instanceof Element && e.target.closest("[data-header]") && grid.current?.contains(e.target)) e.preventDefault();
            }}
            onFocusOutside={(e) => e.preventDefault()}
            onCloseAutoFocus={(e) => {
              e.preventDefault();
              if (document.activeElement === document.body) grid.current?.focus();
            }}
            className="motion-pop z-50 w-[300px] rounded-xl border border-control bg-raised p-3 shadow-[var(--shadow-float)] outline-none"
          >
            <ProfileBody key={col} meta={meta} col={col} scope={scope} done={done} onClose={onClose} />
          </Popover.Content>
        )}
      </Popover.Portal>
    </Popover.Root>
  );
}
