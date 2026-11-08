import { useEffect, useLayoutEffect, useRef, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { activeId, useWorkspace } from "../../state/workspace";
import { MENU_ITEM, MENU_PANEL, cx } from "../../ui/primitives";
import { moveCursor } from "./editorStates";
import { onEditorDocChanged, runActive } from "./runActions";
import {
  choiceTarget,
  closeChooser,
  hasAllRow,
  highlightChoice,
  moveChoice,
  startLines,
  statementLabel,
  useChooser,
  type Choice,
  type OpenChooser,
} from "./chooserState";

const GAP = 4;
const MARGIN = 8;
// Beside the editor's right edge rather than at the cursor, so the panel covers less of the code it previews.
const EDGE = 16;

export function StatementChooser() {
  const chooser = useChooser((s) => s.chooser);

  useEffect(() => onEditorDocChanged(closeChooser), []);
  useEffect(
    () =>
      useWorkspace.subscribe((s, prev) => {
        if (activeId(s) !== activeId(prev)) closeChooser();
      }),
    [],
  );

  return chooser.kind === "open" ? createPortal(<ChooserPanel chooser={chooser} />, document.body) : null;
}

function ChooserPanel({ chooser }: { chooser: OpenChooser }) {
  const ref = useRef<HTMLDivElement>(null);
  const { text, statements, active, anchor } = chooser;
  const lines = startLines(text, statements);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const below = anchor.bottom + GAP;
    const above = anchor.top - GAP - height;
    const fitsBelow = below + height <= window.innerHeight - MARGIN;
    el.style.top = `${fitsBelow || above < MARGIN ? below : above}px`;
    el.style.left = `${Math.max(MARGIN, Math.min(anchor.right, window.innerWidth) - width - EDGE)}px`;
  }, [anchor]);

  useEffect(() => {
    ref.current?.focus();
    const onPointerDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) closeChooser();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => document.removeEventListener("pointerdown", onPointerDown, true);
  }, []);

  useEffect(() => {
    ref.current?.querySelector("[data-highlighted]")?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const run = (choice: Choice) => {
    closeChooser();
    const chosen = choice.kind === "statement" ? statements[choice.index] : undefined;
    if (chosen) moveCursor(chooser.tabId, chosen.to);
    runActive(choiceTarget(statements, choice));
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const digit = /^[1-9]$/.test(e.key) && !e.metaKey && !e.ctrlKey && !e.altKey ? Number(e.key) : 0;
    const moved = moveChoice(active, statements.length, e.key);
    if (e.key === "Escape") closeChooser();
    else if (e.key === "Enter") run(active);
    else if (digit && digit <= statements.length) run({ kind: "statement", index: digit - 1 });
    else if (moved) highlightChoice(moved);
    else if (e.key !== "Tab") return;
    e.preventDefault();
    e.stopPropagation();
  };

  const row = (choice: Choice, testId: string, children: ReactNode) => {
    const on = choice.kind === active.kind && (choice.kind === "all" || (active.kind === "statement" && choice.index === active.index));
    return (
      <div
        key={testId}
        role="option"
        aria-selected={on}
        data-testid={testId}
        data-highlighted={on ? "" : undefined}
        onPointerMove={() => {
          if (!on) highlightChoice(choice);
        }}
        onClick={() => run(choice)}
        className={cx(MENU_ITEM, "text-fg")}
      >
        {children}
      </div>
    );
  };

  return (
    <div
      ref={ref}
      role="listbox"
      aria-label={chooser.unitKind === "Batch" ? "Batch to run" : "Statement to run"}
      tabIndex={-1}
      data-state="open"
      data-testid="statement-chooser"
      onKeyDown={onKeyDown}
      onMouseDown={(e) => e.preventDefault()}
      className={cx(MENU_PANEL, "fixed max-h-[min(60vh,420px)] max-w-[560px] overflow-y-auto outline-none")}
    >
      {statements.map((range, index) =>
        row(
          { kind: "statement", index },
          `statement-choice-${index + 1}`,
          <>
            <span className="w-4 shrink-0 text-right text-ui-12 text-dim tabular-nums">{index + 1}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-ui-12.5">{statementLabel(text, range)}</span>
            <span className="shrink-0 pl-4 text-ui-11.5 text-dim tabular-nums">{`line ${lines[index]}`}</span>
          </>,
        ),
      )}
      {hasAllRow(statements.length) && (
        <>
          <div role="separator" className="my-1 h-px bg-control" />
          {row(
            { kind: "all" },
            "statement-choice-all",
            <>
              <span className="w-4 shrink-0" />
              {`All ${statements.length} ${chooser.unitKind === "Batch" ? "batches" : "statements"}`}
            </>,
          )}
        </>
      )}
    </div>
  );
}
