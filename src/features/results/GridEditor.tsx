import * as Popover from "@radix-ui/react-popover";
import { CalendarIcon, ChevronDownIcon, ClockIcon, ExpandIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { commandForKey } from "../../shell/commands";
import { Button, cx } from "../../ui/primitives";
import { GridTemporalPicker, POPUP_FIELD } from "./GridTemporalPicker";
import type { EditorDescriptor } from "./gridEditorType";
import {
  draftText,
  editOutcome,
  formatJson,
  freshTemporal,
  jsonError,
  parseTemporal,
  temporalError,
  spliceSource,
  textareaChange,
  type EditorDraft,
  type EditOutcome,
  type InitialValue,
} from "./gridEditorValue";

type FinishAction = { kind: "cancel" } | { kind: "stage"; focusGrid: boolean } | { kind: "tab"; back: boolean };

export function GridEditor({
  column,
  initial,
  seed,
  descriptor,
  onFinish,
  onTab,
  onFind,
}: {
  column: string;
  initial: InitialValue;
  seed: { text: string; edited: boolean };
  descriptor: EditorDescriptor;
  onFinish(outcome: EditOutcome, focusGrid: boolean): void;
  onTab(back: boolean, outcome: EditOutcome): void;
  onFind(): void;
}) {
  const [draft, renderDraft] = useState<EditorDraft>({ kind: "raw", ...seed });
  const current = useRef(draft);
  const [opened, setOpened] = useState(seed);
  const setDraft = (next: EditorDraft) => {
    current.current = next;
    renderDraft(next);
  };
  if (opened !== seed) {
    setOpened(seed);
    renderDraft({ kind: "raw", ...seed });
  }
  useLayoutEffect(() => {
    current.current = draft;
  }, [draft]);
  const [surface, setSurface] = useState<"inline" | "popup" | "exact">("inline");
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const inline = useRef<HTMLDivElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const pasteCaret = useRef<number | null>(null);
  const finished = useRef(false);
  const belongs = (target: EventTarget | null) => target instanceof Node && (inline.current?.contains(target) || popup.current?.contains(target));
  useLayoutEffect(() => {
    const focused = document.activeElement;
    if (
      pasteCaret.current !== null &&
      focused instanceof HTMLTextAreaElement &&
      belongs(focused) &&
      (surface === "inline" || popup.current?.contains(focused))
    ) {
      focused.setSelectionRange(pasteCaret.current, pasteCaret.current);
      pasteCaret.current = null;
    }
  }, [draft, surface]);
  useLayoutEffect(() => {
    if (surface !== "inline" && popup.current && !belongs(document.activeElement)) {
      (popup.current.querySelector<HTMLElement>("textarea, input, [data-day][tabindex='0']") ?? popup.current.querySelector<HTMLElement>("button"))?.focus();
    }
  }, [draft.kind, surface]);
  const validation = (value: EditorDraft) => {
    if (!value.edited) return null;
    if (descriptor.kind === "json") return jsonError(draftText(value));
    return descriptor.kind === "temporal" && value.kind === "parts" ? temporalError(value.parts, descriptor.shape) : null;
  };
  const finish = (action: FinishAction = { kind: "stage", focusGrid: false }) => {
    if (finished.current) return true;
    const invalid = action.kind === "cancel" ? null : validation(current.current);
    if (invalid) {
      setError(invalid);
      return false;
    }
    finished.current = true;
    const outcome = action.kind === "cancel" ? ({ kind: "cancel" } satisfies EditOutcome) : editOutcome(initial, current.current);
    if (action.kind === "tab") onTab(action.back, outcome);
    else onFinish(outcome, action.kind === "cancel" || action.focusGrid);
    return true;
  };
  const finishRef = useRef(finish);
  useLayoutEffect(() => {
    finishRef.current = finish;
  });
  useEffect(() => {
    const outside = (event: Event) => {
      if (belongs(event.target)) return;
      if (!finishRef.current()) {
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    };
    const events = ["pointerdown", "mousedown", "click", "contextmenu"];
    for (const event of events) document.addEventListener(event, outside, true);
    return () => {
      for (const event of events) document.removeEventListener(event, outside, true);
    };
  }, []);
  const blur = (target: EventTarget | null) => {
    if (belongs(target)) return;
    // Focus scopes can briefly report no related target while mounting a portal.
    queueMicrotask(() => {
      if (belongs(document.activeElement) || finished.current) return;
      if (!finishRef.current()) (popup.current?.querySelector<HTMLElement>("textarea, input, button") ?? input.current)?.focus();
    });
  };
  const changeText = (text: string) => {
    setDraft({ kind: "raw", text, edited: true });
    setError(null);
  };
  const openPopup = () => {
    const value = current.current;
    if (descriptor.kind === "temporal" && value.kind === "raw") {
      const parts =
        parseTemporal(value.text, descriptor.shape) ?? (!value.edited && !value.text && initial.kind !== "text" ? freshTemporal(descriptor.shape) : null);
      if (parts) setDraft({ kind: "parts", parts, edited: value.edited });
    }
    setSurface("popup");
  };
  const keyDown = (e: KeyboardEvent, inPopup: boolean) => {
    e.stopPropagation();
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Escape") {
      e.preventDefault();
      finish({ kind: "cancel" });
      return;
    }
    const find = (e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "f";
    const command = commandForKey(e);
    if (find || command) {
      e.preventDefault();
      if (finish()) {
        if (find) onFind();
        else command?.run();
      }
      return;
    }
    if (!inPopup && e.key === "Enter") {
      e.preventDefault();
      finish({ kind: "stage", focusGrid: true });
    }
    if (!inPopup && e.key === "Tab") {
      e.preventDefault();
      finish({ kind: "tab", back: e.shiftKey });
    }
    if (!inPopup && e.altKey && e.key === "ArrowDown") {
      e.preventDefault();
      openPopup();
    }
  };
  const text = draftText(draft);
  const invalid = validation(draft);
  const shownError = error ?? invalid;
  const hint =
    descriptor.kind === "text" || descriptor.kind === "raw"
      ? descriptor.hint
      : descriptor.kind === "json"
        ? "JSON source. Format changes whitespace only."
        : "Exact database value";
  const icon =
    descriptor.kind === "temporal" ? (
      descriptor.shape.kind === "time" ? (
        <ClockIcon size={13} />
      ) : (
        <CalendarIcon size={13} />
      )
    ) : descriptor.kind === "enum" ? (
      <ChevronDownIcon size={13} />
    ) : (
      <ExpandIcon size={13} />
    );
  const actions = (
    <>
      <Button onClick={() => finish({ kind: "cancel" })}>Cancel</Button>
      <Button variant="primary" disabled={!!invalid} onClick={() => finish({ kind: "stage", focusGrid: true })}>
        Apply
      </Button>
    </>
  );
  const assisted = descriptor.kind === "temporal" && draft.kind === "parts";
  return (
    <Popover.Root
      open={surface !== "inline"}
      onOpenChange={(open) => {
        if (open) openPopup();
      }}
    >
      <Popover.Anchor asChild>
        <div
          ref={inline}
          data-grid-editor-root
          className="absolute inset-0 z-20 flex bg-bg font-mono text-code text-fg outline outline-1 -outline-offset-1 outline-focus"
          onMouseDown={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.stopPropagation()}
          onKeyDown={(e) => keyDown(e, false)}
          onBlur={(e) => blur(e.relatedTarget)}
        >
          {descriptor.kind === "boolean" ? (
            <div className="flex w-full gap-px bg-control p-px">
              {[
                { label: "True", value: descriptor.trueValue },
                { label: "False", value: descriptor.falseValue },
              ].map(({ label, value }, index) => (
                <button
                  type="button"
                  key={label}
                  autoFocus={index === 0}
                  aria-pressed={(draft.edited || initial.kind === "text") && text === value}
                  className={cx(
                    "min-w-0 flex-1 font-sans text-ui-11 outline-none focus:ring-1 focus:ring-inset focus:ring-focus",
                    (draft.edited || initial.kind === "text") && text === value ? "bg-bg text-fg" : "text-muted hover:bg-hover",
                  )}
                  onClick={() => changeText(value)}
                >
                  {label}
                </button>
              ))}
            </div>
          ) : (
            <>
              <textarea
                ref={input}
                data-testid="grid-editor"
                aria-label={column}
                aria-invalid={!!shownError}
                title={hint}
                autoFocus
                rows={1}
                value={text}
                spellCheck={false}
                inputMode={descriptor.kind === "number" ? descriptor.inputMode : "text"}
                onChange={(e) => changeText(textareaChange(text, e.target.value))}
                onPaste={(e) => {
                  const pasted = e.clipboardData.getData("text/plain");
                  if (!/[\r\n]/.test(pasted)) return;
                  e.preventDefault();
                  const { selectionStart, selectionEnd } = e.currentTarget;
                  pasteCaret.current = selectionStart + pasted.replace(/\r\n?/g, "\n").length;
                  changeText(spliceSource(text, selectionStart, selectionEnd, pasted));
                  setSurface("popup");
                }}
                className="h-full min-w-0 flex-1 resize-none overflow-hidden border-0 bg-transparent px-2 py-0 leading-[inherit] whitespace-pre outline-none"
              />
              {descriptor.kind !== "number" && (
                <button
                  type="button"
                  aria-label={descriptor.kind === "temporal" ? "Open date and time picker" : descriptor.kind === "enum" ? "Choose value" : "Expand value"}
                  title={hint}
                  className="flex w-6 shrink-0 items-center justify-center text-dim hover:bg-hover hover:text-fg"
                  onClick={openPopup}
                >
                  {icon}
                </button>
              )}
            </>
          )}
        </div>
      </Popover.Anchor>
      <Popover.Portal>
        <Popover.Content
          ref={popup}
          data-grid-editor-root
          aria-label={`${column} editor`}
          side="bottom"
          align="start"
          sideOffset={4}
          collisionPadding={8}
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            const target =
              popup.current?.querySelector<HTMLElement>("textarea, input, [data-day][tabindex='0']") ?? popup.current?.querySelector<HTMLElement>("button");
            target?.focus();
            if (target instanceof HTMLTextAreaElement && pasteCaret.current !== null) {
              target.setSelectionRange(pasteCaret.current, pasteCaret.current);
              pasteCaret.current = null;
            }
          }}
          onCloseAutoFocus={(e) => e.preventDefault()}
          onInteractOutside={(e) => e.preventDefault()}
          onEscapeKeyDown={(e) => {
            e.preventDefault();
            finish({ kind: "cancel" });
          }}
          onMouseDown={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.stopPropagation()}
          onKeyDown={(e) => keyDown(e, true)}
          onBlur={(e) => blur(e.relatedTarget)}
          className={cx(
            "motion-pop z-50 max-w-[calc(100vw-16px)] rounded-xl border border-control bg-raised p-4 font-sans text-ui-12 text-fg shadow-[var(--shadow-float)] outline-none",
            descriptor.kind === "temporal" ? "w-[400px]" : "w-[440px]",
          )}
        >
          {descriptor.kind === "temporal" && draft.kind === "parts" ? (
            <GridTemporalPicker
              parts={draft.parts}
              shape={descriptor.shape}
              error={shownError}
              actions={actions}
              onChange={(parts) => {
                setDraft({ kind: "parts", parts, edited: true });
                setError(null);
              }}
              onExact={() => {
                setDraft({ kind: "raw", text, edited: draft.edited });
                setError(null);
              }}
            />
          ) : descriptor.kind === "enum" && surface !== "exact" ? (
            <>
              <input
                aria-label="Search values"
                placeholder="Search values…"
                className={cx(POPUP_FIELD, "mb-2 w-full font-sans")}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
              <div role="listbox" aria-label={column} className="max-h-60 overflow-auto">
                {descriptor.labels
                  .filter((label) => label.toLowerCase().includes(search.toLowerCase()))
                  .map((label) => (
                    <button
                      type="button"
                      role="option"
                      key={label}
                      aria-selected={(draft.edited || initial.kind === "text") && text === label}
                      className="block min-h-8 w-full rounded px-2 text-left whitespace-pre hover:bg-hover aria-selected:bg-accent/15 aria-selected:text-accent"
                      onClick={() => changeText(label)}
                    >
                      {label === "" ? <em className="text-dim">Empty string</em> : label}
                    </button>
                  ))}
                {!descriptor.labels.some((label) => label.toLowerCase().includes(search.toLowerCase())) && <p className="p-2 text-dim">No matching values</p>}
              </div>
              <button
                type="button"
                className="mt-2 text-accent"
                onClick={() => {
                  setSurface("exact");
                }}
              >
                Edit exact value
              </button>
            </>
          ) : (
            <>
              <div className="mb-3 flex items-center justify-between">
                <span>{column}</span>
                {descriptor.kind === "json" && (
                  <button type="button" className="text-accent disabled:text-dim" disabled={!!jsonError(text)} onClick={() => changeText(formatJson(text))}>
                    Format
                  </button>
                )}
              </div>
              <textarea
                aria-label={`Exact ${column}`}
                value={text}
                onPaste={(e) => {
                  e.preventDefault();
                  const { selectionStart, selectionEnd } = e.currentTarget;
                  const pasted = e.clipboardData.getData("text/plain");
                  pasteCaret.current = selectionStart + pasted.replace(/\r\n?/g, "\n").length;
                  changeText(spliceSource(text, selectionStart, selectionEnd, pasted));
                }}
                spellCheck={false}
                onChange={(e) => changeText(textareaChange(text, e.target.value))}
                className="h-52 w-full resize-y rounded-lg border border-control bg-bg p-2 font-mono text-code whitespace-pre outline-none focus:border-focus"
              />
              <p className="mt-2 text-ui-11 text-dim">{hint}</p>
              {descriptor.kind === "temporal" && (
                <button type="button" className="mt-2 text-accent disabled:text-dim" disabled={!parseTemporal(text, descriptor.shape)} onClick={openPopup}>
                  Use picker
                </button>
              )}
            </>
          )}
          {!assisted && shownError && (
            <p role="alert" className="mt-3 text-ui-11 text-danger">
              {shownError}
            </p>
          )}
          {!assisted && <div className="mt-3 flex justify-end gap-2">{actions}</div>}
        </Popover.Content>
      </Popover.Portal>
      {surface === "inline" && shownError && (
        <span role="alert" className="absolute top-full left-0 z-30 w-max max-w-80 rounded border border-danger bg-raised p-2 text-ui-11 text-danger">
          {shownError}
        </span>
      )}
    </Popover.Root>
  );
}
