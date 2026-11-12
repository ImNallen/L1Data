import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { useWorkspace } from "../state/workspace";
import { useReducedMotion } from "./appearance";
import { handleLook } from "./ResizeHandle";

const COLUMNS = {
  sidebar: {
    key: "sidebarWidth",
    reset: 240,
    min: 200,
    max: 480,
    shrink: 1,
    edge: "right",
    handle: "sidebar-resize",
    label: "Resize sidebar",
    line: "bg-hover",
  },
  right: {
    key: "rightPanelWidth",
    reset: 360,
    min: 260,
    max: 560,
    shrink: 1e6,
    edge: "left",
    handle: "right-panel-resize",
    label: "Resize right panel",
    line: "bg-border",
  },
} as const;

export type Column = keyof typeof COLUMNS;

const STEP = 8;

/**
 * `entering` draws the column at zero width for one layout pass so the step to its full width transitions.
 * `opening` and `closing` carry the width transition; `open` has none, so resize drags follow the pointer.
 */
type Phase = "closed" | "entering" | "opening" | "open" | "closing";

function nextPhase(phase: Phase, open: boolean, animate: boolean): Phase {
  if (!animate) return open ? "open" : "closed";
  if (open) return phase === "closed" ? "entering" : phase === "closing" ? "opening" : phase;
  return phase === "entering" ? "closed" : phase === "closed" ? phase : "closing";
}

export const clampWidth = (column: Column, px: number) => Math.round(Math.min(COLUMNS[column].max, Math.max(COLUMNS[column].min, px)));

// The side columns sit outside a panel Group because react-resizable-panels kept them as shares of the window,
// so their widths drifted whenever the window resized. Here the app owns each width in pixels. A column shrinks
// toward its minimum only when the window is too narrow to fit both columns beside the main area. The right panel
// gives way first, so squeezing never re-splits the space between the columns and a drag follows the pointer.
export function PixelColumn({ column, open, children }: { column: Column; open: boolean; children: ReactNode }) {
  const { key, min, max, shrink, edge, handle, label, line } = COLUMNS[column];
  const stored = useWorkspace((s) => s.layout[key]);
  const setLayout = useWorkspace((s) => s.setLayout);
  const motionMs = useWorkspace((s) => s.settings.panelMotionMs);
  const reduced = useReducedMotion();
  // Reduced motion snaps the column open and closed, so no transitionend is awaited.
  const animate = motionMs > 0 && !reduced;
  const [phase, setPhase] = useState<Phase>(open ? "open" : "closed");
  const [wasOpen, setWasOpen] = useState(open);
  if (open !== wasOpen) {
    setWasOpen(open);
    setPhase(nextPhase(phase, open, animate));
  }
  const [dragged, setDragged] = useState<number | null>(null);
  const drag = useRef<{ x: number; from: number; px: number } | null>(null);
  const body = useRef<HTMLDivElement>(null);
  const width = dragged ?? clampWidth(column, stored);
  const toward = edge === "right" ? 1 : -1;

  // Start from the drawn width, which is narrower than the stored one while the window squeezes the column.
  const drawn = () => Math.round(body.current?.getBoundingClientRect().width ?? width);
  const save = (px: number) => {
    if (px !== stored) setLayout({ [key]: px });
  };
  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    const from = drawn();
    drag.current = { x: e.clientX, from, px: from };
    setDragged(from);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current.px = clampWidth(column, drag.current.from + toward * (e.clientX - drag.current.x));
    setDragged(drag.current.px);
  };
  const onPointerEnd = () => {
    if (!drag.current) return;
    if (drag.current.px !== drag.current.from) save(drag.current.px);
    drag.current = null;
    setDragged(null);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowLeft: -STEP, ArrowRight: STEP }[e.key];
    if (step === undefined) return;
    e.preventDefault();
    save(clampWidth(column, drawn() + toward * step));
  };

  useLayoutEffect(() => {
    if (phase !== "entering") return;
    body.current?.getBoundingClientRect();
    // oxlint-disable-next-line react/set-state-in-effect -- the forced layout commits the entering frame, so the switch to opening runs the transition
    setPhase("opening");
  }, [phase]);
  const settle = () => setPhase((p) => (p === "opening" ? "open" : p === "closing" ? "closed" : p));
  // transitionend is skipped when nothing paints, as in a hidden window, so a timer settles the phase too.
  useEffect(() => {
    if (phase !== "opening" && phase !== "closing") return;
    const timer = setTimeout(settle, motionMs + 50);
    return () => clearTimeout(timer);
  }, [phase, motionMs]);

  if (phase === "closed") return null;
  const sliding = phase !== "open";

  const separator = (
    <div
      id={handle}
      data-testid={handle}
      role="separator"
      aria-label={label}
      title={`${label}. Double-click to reset size.`}
      aria-description="Use arrow keys to resize. Double-click to reset size."
      aria-orientation="vertical"
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      data-separator={dragged === null ? undefined : "active"}
      className={`${handleLook("vertical", line)} shrink-0 cursor-ew-resize`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerEnd}
      onPointerCancel={onPointerEnd}
      onKeyDown={onKeyDown}
      onDoubleClick={() => save(COLUMNS[column].reset)}
    />
  );
  const content = (
    <div
      ref={body}
      data-testid={column}
      data-phase={sliding ? phase : undefined}
      className="h-full overflow-hidden"
      style={{
        width: phase === "entering" || phase === "closing" ? 0 : width,
        minWidth: sliding ? 0 : min,
        flexShrink: shrink,
        transition: phase === "opening" || phase === "closing" ? "width var(--panel-duration) var(--panel-ease)" : undefined,
      }}
      onTransitionEnd={(e) => {
        if (e.target === e.currentTarget && e.propertyName === "width") settle();
      }}
    >
      <div className="h-full" style={sliding ? { width } : undefined}>
        {children}
      </div>
    </div>
  );
  return edge === "right" ? (
    <>
      {content}
      {separator}
    </>
  ) : (
    <>
      {separator}
      {content}
    </>
  );
}
