import { ArrowUpIcon, LoaderCircleIcon, XIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef } from "react";
import { createPortal } from "react-dom";
import type { ChatId } from "../../ipc/bindings";
import { MAC } from "../../shell/hostedWindow";
import { ReplyText } from "../../shell/lazySurfaces";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { cx } from "../../ui/primitives";
import { applyAsk, discardAsk, dismissAsk, openAskAi, setAskPrompt, submitAsk, useAskAi, type AskCapture, type AskState } from "./askAi";
import { changedHunks } from "./askDiff";
import { askAnchor, askPreviewHeight, onEditorGeometry } from "./editorStates";

const MARGIN = 16;
const SHORTCUT = MAC ? "⌘J" : "Ctrl+J";

export function askContext(capture: AskCapture): string {
  if (capture.source.scope === "Cell") return capture.source.sql.trim() ? "This cell" : "Empty cell";
  const first = capture.doc.lineAt(capture.range.from).number;
  const last = capture.doc.lineAt(Math.max(capture.range.from, capture.range.to - 1)).number;
  return first === last ? `Selection L${first}` : `Selection L${first}–${last}`;
}

export function AskAi() {
  const state = useAskAi((s) => s.state);
  const available = useWorkspace((s) => s.screen.kind === "workspace" && s.overlay.kind === "none");
  return state.kind === "closed" || !available ? null : createPortal(<AskSurface state={state} />, document.body);
}

function AskThread({ chatId, capture }: { chatId: ChatId; capture: AskCapture }) {
  const root = useRef<HTMLDivElement>(null);
  const transcript = useLive((s) => s.transcripts[chatId]);
  const entries = useMemo(() => Object.values(transcript ?? {}).sort((a, b) => a.seq - b.seq), [transcript]);

  useLayoutEffect(() => {
    const element = root.current;
    const latest = element ? [...element.querySelectorAll<HTMLElement>("[data-ask-request]")].at(-1) : undefined;
    if (!element || !latest) return;
    const top = latest.offsetTop - parseFloat(getComputedStyle(element).paddingTop);
    element.style.height = `${element.scrollHeight - top}px`;
    element.scrollTop = top;
  }, [entries]);

  if (!entries.some(({ entry }) => entry.kind === "User")) return null;
  return (
    <div ref={root} data-testid="ask-ai-thread" className="relative mt-1 flex max-h-[min(360px,50vh)] flex-col gap-2.5 overflow-y-auto px-0.5 pt-1 pb-1">
      {entries.map(({ seq, entry }) =>
        entry.kind === "User" ? (
          <p
            key={seq}
            data-ask-request
            className="selectable max-w-[85%] self-end rounded-md bg-selected px-2 py-1 text-ui-13 break-words whitespace-pre-wrap text-fg"
          >
            {entry.text}
          </p>
        ) : entry.kind === "Text" ? (
          <ReplyText
            key={seq}
            testId={`ask-ai-text-${seq}`}
            blocks={entry.blocks}
            db={capture.db}
            databaseKind={capture.databaseKind}
            cards={false}
            streaming={!entry.done}
          />
        ) : null,
      )}
    </div>
  );
}

function AskSurface({ state }: { state: Exclude<AskState, { kind: "closed" }> }) {
  const root = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const { capture } = state.session;
  const review = state.kind === "review";
  const sending = state.kind === "sending";

  useLayoutEffect(() => {
    let frame = 0;
    const measure = () => {
      const element = root.current;
      if (!element) return;
      const first = review && state.session.draft !== null ? changedHunks(capture.doc, capture.range, state.session.draft)[0] : undefined;
      const position = first?.from ?? capture.anchor;
      const end = first?.to ?? (capture.source.scope === "Selection" ? capture.range.to : position);
      const anchor = askAnchor(capture.tabId, position, end);
      if (!anchor) return;
      const box = element.getBoundingClientRect();
      const beside = anchor.left + 24;
      const fitsBeside = beside + box.width <= window.innerWidth - MARGIN;
      const below = anchor.bottom + 6;
      const above = anchor.top - box.height - 6;
      const top = fitsBeside ? anchor.top - 10 : below + box.height <= window.innerHeight - MARGIN ? below : above;
      element.style.left = `${Math.max(MARGIN, Math.min(beside, window.innerWidth - box.width - MARGIN))}px`;
      element.style.top = `${Math.max(MARGIN, Math.min(top, window.innerHeight - box.height - MARGIN))}px`;
      const height = askPreviewHeight(capture.tabId);
      if (height !== useAskAi.getState().previewHeight) useAskAi.setState({ previewHeight: height });
    };
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    const resize = new ResizeObserver(schedule);
    if (root.current) resize.observe(root.current);
    const editor = document.querySelector("[data-testid=sql-editor]");
    if (editor) resize.observe(editor);
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", schedule, true);
    const geometry = onEditorGeometry(schedule);
    schedule();
    return () => {
      cancelAnimationFrame(frame);
      resize.disconnect();
      geometry();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
    };
  }, [capture, review, state.session.draft]);

  useEffect(() => {
    if (!review && !sending) input.current?.focus();
  }, [review, sending, state.session.id]);

  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.isComposing) return;
      event.preventDefault();
      event.stopPropagation();
      dismissAsk();
    };
    window.addEventListener("keydown", escape, true);
    return () => window.removeEventListener("keydown", escape, true);
  }, []);

  useEffect(() => {
    if (review) return;
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) dismissAsk();
    };
    document.addEventListener("pointerdown", outside, true);
    return () => document.removeEventListener("pointerdown", outside, true);
  }, [review]);

  if (review)
    return (
      <div
        ref={root}
        role="toolbar"
        aria-label="Review AI changes"
        data-testid="ask-ai-review"
        className="fixed z-50 flex items-center gap-0.5 rounded-[5px] border border-control bg-raised px-1 py-[3px] text-ui-12 font-medium shadow-sm"
      >
        <button className="flex h-6 items-center gap-2 rounded px-2 hover:bg-hover" onClick={openAskAi}>
          Ask AI <span className="text-dim">{SHORTCUT}</span>
        </button>
        <button className="h-6 rounded px-2 text-muted hover:bg-hover" onClick={discardAsk}>
          Discard
        </button>
        <button className="h-6 rounded border border-control bg-raised px-2 text-fg hover:bg-hover" onClick={applyAsk}>
          {capture.source.scope === "Cell" && !capture.source.sql.trim() ? "Insert SQL" : "Apply"}
        </button>
      </div>
    );

  const message = state.kind === "error" || state.kind === "prompt" ? state.message : null;
  return (
    <div
      ref={root}
      role="dialog"
      aria-label="Ask AI"
      data-testid="ask-ai-popover"
      className="fixed z-50 w-[440px] max-w-[calc(100vw-32px)] rounded-md border border-control bg-raised px-2 pt-1.5 pb-2 shadow-sm"
    >
      <div className="flex h-5 items-center justify-between pl-0.5 text-ui-11 text-muted">
        <span>Ask AI · {askContext(capture)}</span>
        <button
          aria-label={sending ? "Stop request" : "Close Ask AI"}
          className="flex size-5 items-center justify-center rounded hover:bg-hover"
          onClick={dismissAsk}
        >
          <XIcon size={12} />
        </button>
      </div>
      {state.session.chatId && <AskThread chatId={state.session.chatId} capture={capture} />}
      <form
        className="mt-1.5 flex items-end gap-2 rounded-md border border-control bg-hover py-1 pr-1 pl-2 focus-within:border-focus"
        onSubmit={(event) => {
          event.preventDefault();
          void submitAsk();
        }}
      >
        <textarea
          ref={input}
          aria-label="Ask a question or describe a change"
          placeholder="Ask a question or describe a change…"
          rows={1}
          disabled={sending}
          value={state.session.prompt}
          onChange={(event) => {
            setAskPrompt(event.target.value);
            event.target.style.height = "auto";
            event.target.style.height = `${Math.min(event.target.scrollHeight, 160)}px`;
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              void submitAsk();
            }
          }}
          className="min-h-6 flex-1 resize-none bg-transparent py-0.5 text-ui-13 leading-5 text-fg outline-none placeholder:text-muted disabled:text-muted"
        />
        <button
          type="submit"
          aria-label={sending ? "Asking AI" : "Send to AI"}
          disabled={sending || !state.session.prompt.trim()}
          className="grid size-6 shrink-0 place-items-center rounded-full bg-fg text-bg transition-colors hover:opacity-85 disabled:pointer-events-none disabled:opacity-40"
        >
          {sending ? <LoaderCircleIcon size={13} className="animate-spin" /> : <ArrowUpIcon size={14} strokeWidth={2.25} />}
        </button>
      </form>
      {message && (
        <p
          role={state.kind === "error" ? "alert" : "status"}
          className={cx("px-0.5 pt-1 pb-0.5 text-ui-12", state.kind === "error" ? "text-danger" : "text-muted")}
        >
          {message}
        </p>
      )}
    </div>
  );
}
