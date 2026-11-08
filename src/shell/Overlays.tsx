import { useEffect, useRef, useState } from "react";
import { ParamsDialog } from "../features/editor/ParamsDialog";
import { ImportDialog } from "../features/results/ImportDialog";
import { RecordView } from "../features/results/RecordView";
import { UsageDialog } from "../features/usage/UsageDialog";
import { useWorkspace, type Overlay } from "../state/workspace";
import { Button, Modal } from "../ui/primitives";

function Toast() {
  const toast = useWorkspace((s) => s.toast);
  const [visible, setVisible] = useState<number | null>(null);
  useEffect(() => {
    if (!toast) return;
    // oxlint-disable-next-line react/set-state-in-effect -- each new toast shows for 4 s
    setVisible(toast.id);
    const t = setTimeout(() => setVisible(null), 4_000);
    return () => clearTimeout(t);
  }, [toast]);
  const shown = toast && visible === toast.id ? toast.text : null;
  // The live region stays mounted and only its text changes: many screen readers skip a region that mounts with its text.
  return (
    <>
      <div role="status" data-testid="toast-announcement" className="sr-only">
        {shown}
      </div>
      {shown !== null && (
        <div
          data-testid="toast"
          aria-hidden
          className="fixed bottom-10 left-1/2 z-50 max-w-[520px] -translate-x-1/2 rounded-lg border border-control bg-raised px-3.5 py-2 text-ui-13 text-fg shadow-[var(--shadow-popover)]"
        >
          {shown}
        </div>
      )}
    </>
  );
}

/**
 * Keyed by `seq`, so a question that follows another remounts and takes focus again. Enter picks the primary choice.
 * A question with a danger choice and no primary opens on Cancel, its safe answer.
 */
function AskDialog({ question }: { question: Extract<Overlay, { kind: "ask" }> }) {
  const answer = useWorkspace((s) => s.answer);
  const close = useWorkspace((s) => s.close);
  const primary = useRef<HTMLButtonElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const has = (variant: "primary" | "danger") => question.choices.some((c) => c.variant === variant);
  return (
    <Modal
      open
      onOpenChange={(open) => !open && close()}
      title={question.title}
      description={question.body}
      width={440}
      testId={`${question.testId}-dialog`}
      role="alertdialog"
      initialFocus={has("primary") ? primary : has("danger") ? cancel : undefined}
    >
      {question.statements.length > 0 && (
        <div data-testid={`${question.testId}-statements`} className="-mt-1 mb-4 flex flex-col gap-1.5">
          {question.statements.map((sql, i) => (
            <pre
              key={i}
              className="selectable max-h-24 overflow-auto rounded-md border border-control bg-bg px-2.5 py-1.5 font-mono text-code-sm whitespace-pre-wrap text-fg [overflow-wrap:anywhere]"
            >
              {sql}
            </pre>
          ))}
        </div>
      )}
      {question.rows.length > 0 && (
        <ul data-testid={`${question.testId}-rows`} className="-mt-1 mb-4 flex flex-col gap-1.5">
          {question.rows.map((row, i) => (
            <li key={i} className="flex items-baseline justify-between gap-4 text-ui-13">
              <span className="min-w-0 truncate text-fg">{row.label}</span>
              <span className="shrink-0 text-ui-12 text-dim">{row.detail}</span>
            </li>
          ))}
        </ul>
      )}
      {question.note && <p className="-mt-1 mb-4 text-ui-12 text-dim">{question.note}</p>}
      <div className="flex justify-end gap-2">
        <Button ref={cancel} variant="ghost" data-testid={`${question.testId}-cancel`} onClick={close}>
          Cancel
        </Button>
        {question.choices.map((c) => (
          <Button
            key={c.id}
            ref={c.variant === "primary" ? primary : undefined}
            data-testid={`${question.testId}-${c.id}`}
            variant={c.variant}
            onClick={() => answer(c.id)}
          >
            {c.label}
          </Button>
        ))}
      </div>
    </Modal>
  );
}

export function AskOverlay() {
  const overlay = useWorkspace((s) => s.overlay);
  return overlay.kind === "ask" ? <AskDialog key={overlay.seq} question={overlay} /> : null;
}

export function Overlays() {
  const overlay = useWorkspace((s) => s.overlay);
  const inSettings = useWorkspace((s) => s.screen.kind === "settings");
  const close = useWorkspace((s) => s.close);
  const onOpenChange = (open: boolean) => !open && close();

  return (
    <>
      <ParamsDialog />
      <ImportDialog />
      <UsageDialog />
      {!inSettings && <AskOverlay />}
      {overlay.kind === "record" && <RecordView key={`${overlay.meta.ref.runId}:${overlay.row}`} meta={overlay.meta} row={overlay.row} onClose={close} />}
      <Modal open={overlay.kind === "cell"} onOpenChange={onOpenChange} title={overlay.kind === "cell" ? overlay.title : ""} width={720} testId="cell-dialog">
        {overlay.kind === "cell" && (
          <pre className="selectable max-h-[52vh] overflow-auto rounded-lg border border-control bg-bg p-3 font-mono text-ui-12.5 leading-5 whitespace-pre-wrap text-fg">
            {overlay.value ?? <span className="text-dim">NULL</span>}
          </pre>
        )}
      </Modal>
      <Toast />
    </>
  );
}
