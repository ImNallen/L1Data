import * as Dialog from "@radix-ui/react-dialog";
import { BookmarkIcon, ClockIcon, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { commands } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace, type QueriesOverlay, type QueriesTab } from "../../state/workspace";
import { cx, DialogSurface, Kbd } from "../../ui/primitives";
import { focusEditor } from "../editor/editorStates";
import { HistoryBrowser } from "../history/HistoryBrowser";
import { firstPageQuery } from "../history/historyList";
import { SavedBrowser } from "../saved/SavedBrowser";

const TABS: { tab: QueriesTab; label: string; icon: LucideIcon }[] = [
  { tab: "history", label: "History", icon: ClockIcon },
  { tab: "saved", label: "Saved", icon: BookmarkIcon },
];

/** Every query in history, whatever the History tab's search narrows its own counts to. Read only while `open`. */
function useHistoryTotal(open: boolean): number | null {
  const tail = useLive((s) => s.historyTail);
  const [total, setTotal] = useState<number | null>(null);
  useEffect(() => {
    if (!open) return;
    let current = true;
    commands
      .historyBrowse({ ...firstPageQuery("", "All"), limit: 1 })
      .then((res) => {
        if (current && res.status === "ok") setTotal(res.data.counts.all);
      })
      // The count is a hint. The History tab reports its own load errors.
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [open, tail]);
  return total;
}

function QueriesSwitch({ tab, counts, locked, onClose }: { tab: QueriesTab; counts: Record<QueriesTab, number | null>; locked: boolean; onClose: () => void }) {
  const openQueries = useWorkspace((s) => s.openQueries);
  return (
    <>
      <div role="group" aria-label="Show" className="flex h-8 shrink-0 gap-0.5 rounded-lg bg-segment-track p-[3px] shadow-[inset_0_1px_2px_rgb(0_0_0/0.06)]">
        {TABS.map(({ tab: value, label, icon: Icon }) => (
          <button
            key={value}
            type="button"
            data-testid={`queries-tab-${value}`}
            aria-pressed={tab === value}
            disabled={locked}
            onClick={() => tab !== value && openQueries(value)}
            className={cx(
              "flex items-center gap-1.5 rounded-md border-[0.5px] px-2 text-ui-12 outline-none focus-visible:ring-1 focus-visible:ring-focus disabled:opacity-40",
              tab === value
                ? "border-segment-thumb-border bg-segment-thumb font-medium text-fg shadow-[0_1px_2px_rgb(0_0_0/0.08)]"
                : "border-transparent text-muted enabled:hover:text-fg",
            )}
          >
            <Icon size={13} aria-hidden />
            {label}
            <span className="font-normal text-dim tabular-nums">{counts[value]?.toLocaleString() ?? "…"}</span>
          </button>
        ))}
      </div>
      <button
        type="button"
        data-testid="queries-close"
        aria-label="Close queries"
        onClick={onClose}
        className="ml-1 border-l border-control pl-3 text-dim outline-none hover:text-fg focus-visible:ring-1 focus-visible:ring-focus"
      >
        <Kbd>esc</Kbd>
      </button>
    </>
  );
}

/**
 * Query history and Saved queries in one dialog. A tab switch swaps the pane inside the same surface, so the backdrop
 * and the dialog stay put. The pane remounts on each new overlay, which resets its search.
 */
export function QueriesDialog() {
  const overlay = useWorkspace((s) => (s.overlay.kind === "queries" ? s.overlay : null));
  const close = useWorkspace((s) => s.close);
  // The last overlay keeps rendering while the dialog animates closed.
  const [shown, setShown] = useState<{ overlay: QueriesOverlay; key: number }>({ overlay: { kind: "queries", tab: "history" }, key: 0 });
  if (overlay && overlay !== shown.overlay) setShown({ overlay, key: shown.key + 1 });
  const restoreFocus = useRef(false);
  const holdOpen = useRef<(() => boolean) | null>(null);
  const counts = { history: useHistoryTotal(overlay !== null), saved: useLive((s) => s.savedQueries.items.length) };

  const closeToEditor = () => {
    restoreFocus.current = true;
    close();
  };
  const requestClose = () => {
    if (!holdOpen.current?.()) close();
  };
  const hold = (e: Event) => {
    if (holdOpen.current?.()) e.preventDefault();
  };
  const current = shown.overlay;
  const controls = (locked: boolean) => <QueriesSwitch tab={current.tab} counts={counts} locked={locked} onClose={requestClose} />;
  return (
    <Dialog.Root open={overlay !== null} onOpenChange={(open) => !open && close()}>
      <Dialog.Portal>
        <DialogSurface
          aria-describedby={undefined}
          data-testid="queries-dialog"
          onEscapeKeyDown={hold}
          onPointerDownOutside={hold}
          onCloseAutoFocus={(event) => {
            if (!restoreFocus.current) return;
            event.preventDefault();
            restoreFocus.current = false;
            focusEditor();
          }}
          className="flex h-[640px] w-[960px] flex-col overflow-hidden"
        >
          <Dialog.Title className="sr-only">{current.tab === "history" ? "Query history" : "Saved queries"}</Dialog.Title>
          {current.tab === "history" ? (
            <HistoryBrowser key={shown.key} closeToEditor={closeToEditor} controls={controls(false)} />
          ) : (
            <SavedBrowser key={shown.key} entry={current.entry} close={close} closeToEditor={closeToEditor} holdOpen={holdOpen} controls={controls} />
          )}
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
