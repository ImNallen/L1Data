import { ChevronDownIcon, ChevronUpIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type KeyboardEvent } from "react";
import type { ResultMeta } from "../../ipc/bindings";
import { useCatalog } from "../../state/catalog";
import { rowCacheFor } from "../../state/rowCache";
import { IconButton, Modal } from "../../ui/primitives";
import { groupCount } from "./format";
import { HeaderIcon, primaryKeyColumns } from "./ResultGrid";

const STEP: Record<string, number> = { ArrowUp: -1, ArrowDown: 1 };
const FULL_VALUE_DEBOUNCE_MS = 120;
const NAV_BUTTON = "aria-disabled:cursor-default aria-disabled:opacity-40 aria-disabled:hover:bg-transparent aria-disabled:hover:text-muted";

function Value({ value, cut }: { value: string | null | undefined; cut: boolean }) {
  if (value === undefined) return null;
  if (value === null || value === "")
    return <span className="rounded-[3px] border border-faint px-1 font-sans text-ui-11 leading-4 text-dim italic">{value === null ? "NULL" : "empty"}</span>;
  return (
    <span className="whitespace-pre-wrap text-fg [overflow-wrap:anywhere]">
      {value}
      {cut && <span className="text-dim">…</span>}
    </span>
  );
}

export function RecordView({ meta, row: first, onClose }: { meta: ResultMeta; row: number; onClose: () => void }) {
  const [cache] = useState(() => rowCacheFor(meta.ref));
  useSyncExternalStore(cache.subscribe, cache.getVersion);
  const [row, setRow] = useState(first);
  const [full, setFull] = useState<{ row: number; values: Record<number, string | null> }>({ row: first, values: {} });
  const views = useCatalog((s) => s.views);
  const pk = useMemo(() => primaryKeyColumns(views, meta), [views, meta]);
  const fields = useRef<HTMLDivElement>(null);
  const last = meta.rowCount - 1;

  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void cache.ensure({ start: row, end: row + 1 }, meta.rowCount).then(() => {
      if (!live) return;
      timer = setTimeout(() => {
        for (let col = 0; col < meta.columns.length; col++) {
          if (!cache.isCut(row, col)) continue;
          void cache.loadFull(row, col).then((value) => {
            if (live && value !== undefined) setFull((f) => ({ row, values: { ...(f.row === row ? f.values : {}), [col]: value } }));
          });
        }
      }, FULL_VALUE_DEBOUNCE_MS);
    });
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [cache, row, meta]);

  const values = cache.get(row);
  const loaded = full.row === row ? full.values : {};
  const move = (by: number) => setRow((r) => Math.max(0, Math.min(last, r + by)));
  const onKeyDown = (e: KeyboardEvent) => {
    const by = STEP[e.key];
    if (by === undefined || e.shiftKey || e.altKey || e.metaKey || e.ctrlKey) return;
    e.preventDefault();
    move(by);
  };

  return (
    <Modal open onOpenChange={(open) => !open && onClose()} title="Record" width={720} testId="record-dialog" initialFocus={fields} onKeyDown={onKeyDown}>
      <div className="-mt-2 mb-3 flex items-center gap-1 text-ui-12.5 text-dim">
        <span data-testid="record-position" aria-live="polite" aria-atomic className="tabular-nums">
          Row {groupCount(row + 1)} of {groupCount(meta.rowCount)}
        </span>
        <div className="flex-1" />
        <IconButton
          data-testid="record-prev"
          aria-label="Previous row"
          title="Previous row (↑)"
          aria-disabled={row === 0}
          className={NAV_BUTTON}
          onClick={() => move(-1)}
        >
          <ChevronUpIcon size={15} />
        </IconButton>
        <IconButton
          data-testid="record-next"
          aria-label="Next row"
          title="Next row (↓)"
          aria-disabled={row === last}
          className={NAV_BUTTON}
          onClick={() => move(1)}
        >
          <ChevronDownIcon size={15} />
        </IconButton>
      </div>
      {cache.gone ? (
        <p className="text-ui-13 text-dim">This result is no longer available. Run the query again.</p>
      ) : (
        <div
          ref={fields}
          data-testid="record-fields"
          data-row={row}
          role="region"
          aria-label="Fields"
          tabIndex={0}
          className="selectable grid max-h-[calc(72vh-8rem)] grid-cols-[minmax(140px,max-content)_1fr] overflow-auto rounded-lg border border-control bg-bg outline-none focus-visible:border-focus"
        >
          {meta.columns.map((c, col) => {
            const cut = !(col in loaded) && cache.isCut(row, col);
            return (
              <div key={col} data-testid={`record-field-${col}`} className="contents">
                <div className="flex min-w-0 items-start gap-1.5 border-b border-grid px-3 py-1.5" title={c.type.display}>
                  <span className="mt-[3px]">
                    <HeaderIcon column={c} primaryKey={pk[col]!} />
                  </span>
                  <span className="min-w-0 truncate text-ui-12.5 font-medium text-muted">{c.name}</span>
                </div>
                <div data-testid={`record-value-${col}`} className="min-w-0 border-b border-grid px-3 py-1.5 font-mono text-code">
                  <Value value={col in loaded ? loaded[col] : values?.[col]} cut={cut} />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </Modal>
  );
}
