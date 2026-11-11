import { GripVerticalIcon } from "lucide-react";
import { useEffect, useRef, useState, type PointerEvent } from "react";
import { createPortal, flushSync } from "react-dom";
import type { TabId } from "../../ipc/bindings";
import { useWorkspace } from "../../state/workspace";
import { focusActiveCell } from "../notebooks/cellNavigation";
import { clamp } from "./cellSizing";

const START_PX = 4;
const EDGE_PX = 48;
const MAX_SCROLL_PX = 16;

type Press = { x: number; y: number; startY: number; scroller: HTMLElement; cell: HTMLElement; moved: boolean; from: number; to: number };
type Drop = { ghost: { top: number; left: number }; line: { top: number; left: number; width: number } | null };

/**
 * Where a pointer at `y` drops the cell at `from`: after every cell whose middle is above it. `line` is the drop line's
 * viewport y halfway between the cells it falls between, or `null` when the cell would stay where it is.
 */
function dropAt(scroller: HTMLElement, y: number, from: number): { index: number; line: number | null } {
  const rects = [...scroller.querySelectorAll(".notebook-cell")].map((el) => el.getBoundingClientRect());
  const gap = rects.filter((r) => r.top + r.height / 2 < y).length;
  const index = gap > from ? gap - 1 : gap;
  const [first, second] = rects;
  if (index === from || !first) return { index, line: null };
  const half = second ? (second.top - first.bottom) / 2 : 12;
  const view = scroller.getBoundingClientRect();
  const line = gap === 0 ? first.top - half : rects[gap - 1]!.bottom + half;
  return { index, line: clamp(line, view.top + 1, view.bottom - 1) };
}

export function CellGrip({ id, index, title }: { id: TabId; index: number; title: string }) {
  const press = useRef<Press | null>(null);
  const [drop, setDrop] = useState<Drop | null>(null);
  const number = String(index + 1).padStart(2, "0");
  const follow = () => {
    const p = press.current;
    if (!p) return;
    const { index: to, line } = dropAt(p.scroller, p.y, p.from);
    p.to = to;
    const view = p.scroller.getBoundingClientRect();
    const { left, width } = p.cell.getBoundingClientRect();
    setDrop({ ghost: { top: clamp(p.y - 16, view.top, view.bottom - 32), left: p.x + 14 }, line: line === null ? null : { top: line, left, width } });
  };
  const end = () => {
    press.current?.cell.removeAttribute("data-dragging");
    press.current = null;
    setDrop(null);
  };
  const dragging = drop !== null;
  useEffect(() => {
    if (!dragging) return;
    let frame = requestAnimationFrame(function scroll() {
      const p = press.current;
      if (!p) return;
      const view = p.scroller.getBoundingClientRect();
      const into = Math.max(view.top + EDGE_PX - p.y, 0) - Math.max(p.y - (view.bottom - EDGE_PX), 0);
      if (into !== 0) {
        p.scroller.scrollTop -= Math.sign(into) * Math.min(Math.abs(into) / EDGE_PX, 1) * MAX_SCROLL_PX;
        follow();
      }
      frame = requestAnimationFrame(scroll);
    });
    const cancel = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      end();
    };
    window.addEventListener("keydown", cancel, true);
    window.addEventListener("blur", end);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("keydown", cancel, true);
      window.removeEventListener("blur", end);
    };
  }, [dragging]);
  const onPointerDown = (e: PointerEvent<HTMLSpanElement>) => {
    const scroller = e.currentTarget.closest<HTMLElement>(".notebook-workspace");
    const cell = e.currentTarget.closest<HTMLElement>(".notebook-cell");
    if (e.button !== 0 || !scroller || !cell) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    press.current = { x: e.clientX, y: e.clientY, startY: e.clientY, scroller, cell, moved: false, from: index, to: index };
  };
  const onPointerMove = (e: PointerEvent<HTMLSpanElement>) => {
    const p = press.current;
    if (!p) return;
    p.x = e.clientX;
    p.y = e.clientY;
    if (!p.moved && Math.abs(p.y - p.startY) < START_PX) return;
    if (!p.moved) p.cell.setAttribute("data-dragging", "");
    p.moved = true;
    follow();
  };
  const onPointerUp = () => {
    const p = press.current;
    end();
    if (!p?.moved || p.to === p.from) return;
    const hadFocus = p.scroller.contains(document.activeElement);
    // The drop makes this cell active, which takes the shared editor out of the cell that held focus.
    flushSync(() => useWorkspace.getState().moveCell(id, p.to));
    if (hadFocus) focusActiveCell();
  };
  return (
    <span
      data-testid={`cell-grip-${id}`}
      data-dragging={dragging || undefined}
      className="notebook-cell-grip"
      title="Drag to move this cell"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={end}
      onLostPointerCapture={end}
    >
      <span className="font-mono text-ui-11 text-dim tabular-nums">{number}</span>
      <GripVerticalIcon size={13} aria-hidden />
      {drop &&
        createPortal(
          <>
            <div className="notebook-drag-ghost" style={drop.ghost}>
              <span className="font-mono text-ui-11 text-dim tabular-nums">{number}</span>
              <span className="min-w-0 truncate">{title}</span>
            </div>
            {drop.line && <div data-testid="cell-drop-line" className="notebook-drop-line" style={drop.line} />}
          </>,
          document.body,
        )}
    </span>
  );
}
