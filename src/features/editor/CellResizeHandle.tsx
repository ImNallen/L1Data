import { useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { flushSync } from "react-dom";
import { handleLook } from "../../shell/ResizeHandle";
import { clamp } from "./cellSizing";

const STEP = 8;
const DRAG_SLACK = "--notebook-drag-slack";

let stopTrimming = () => {};

/** Trims the drag slack to the room that holds scrollTop, and drops it once the notebook scrolls back above that room. */
function releaseSlack(notebook: HTMLElement) {
  const trim = () => {
    const slack = parseFloat(notebook.style.getPropertyValue(DRAG_SLACK)) || 0;
    const room = notebook.scrollTop + notebook.clientHeight - (notebook.scrollHeight - slack);
    if (room > 0) notebook.style.setProperty(DRAG_SLACK, `${room}px`);
    else stopTrimming();
  };
  stopTrimming = () => {
    notebook.removeEventListener("scroll", trim);
    notebook.style.removeProperty(DRAG_SLACK);
    stopTrimming = () => {};
  };
  notebook.addEventListener("scroll", trim, { passive: true });
  trim();
}

type CellResizeHandleProps = {
  label: string;
  testId: string;
  value: number;
  min: number;
  max: number;
  lineColor: string;
  onResize(px: number | null): void;
};

export function CellResizeHandle({ label, testId, value, min, max, lineColor, onResize }: CellResizeHandleProps) {
  const [dragging, setDragging] = useState(false);
  const drag = useRef<{ y: number; from: number; moved: boolean } | null>(null);
  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { y: e.clientY, from: value, moved: false };
    setDragging(true);
    const notebook = e.currentTarget.closest<HTMLElement>(".notebook-workspace");
    const retained = parseFloat(notebook?.style.getPropertyValue(DRAG_SLACK) ?? "") || 0;
    stopTrimming();
    // Shrinking the last cell would pull the scroller's end up and clamp scrollTop, sliding the handle away from the pointer.
    // Slack an earlier drag left behind may be what holds scrollTop now, so this drag's allowance adds to it.
    notebook?.style.setProperty(DRAG_SLACK, `${retained + value - min}px`);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    if (!drag.current.moved && e.clientY === drag.current.y) return;
    drag.current.moved = true;
    onResize(clamp(drag.current.from + e.clientY - drag.current.y, min, max));
  };
  const onPointerEnd = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current = null;
    setDragging(false);
    const notebook = e.currentTarget.closest<HTMLElement>(".notebook-workspace");
    if (notebook) releaseSlack(notebook);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowUp: -STEP, ArrowDown: STEP }[e.key];
    if (step === undefined) return;
    e.preventDefault();
    flushSync(() => onResize(clamp(value + step, min, max)));
    e.currentTarget.scrollIntoView({ block: "nearest" });
  };
  return (
    <div
      data-testid={testId}
      role="separator"
      aria-label={label}
      title={`${label}. Double-click to reset size.`}
      aria-description="Use arrow keys to resize. Double-click to reset size."
      aria-orientation="horizontal"
      aria-valuenow={value}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      data-separator={dragging ? "active" : undefined}
      className={`${handleLook("horizontal", lineColor)} cursor-ns-resize`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerEnd}
      onPointerCancel={onPointerEnd}
      onKeyDown={onKeyDown}
      onDoubleClick={() => onResize(null)}
    />
  );
}
