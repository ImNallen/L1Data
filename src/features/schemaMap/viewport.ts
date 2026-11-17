export type Point = { x: number; y: number };
export type Rect = Point & { width: number; height: number };
export type Size = { width: number; height: number };
export type Viewport = Point & { scale: number };

const MARGIN = 36;
const MIN_SCALE = 0.001;
const MAX_SCALE = 2.5;

export function fitViewport(bounds: Rect, box: Size, { maxScale }: { maxScale: number }): Viewport {
  const fit = Math.min((box.width - 2 * MARGIN) / bounds.width, (box.height - 2 * MARGIN) / bounds.height);
  const scale = Math.min(maxScale, Math.max(MIN_SCALE, fit));
  return {
    x: (box.width - bounds.width * scale) / 2 - bounds.x * scale,
    y: (box.height - bounds.height * scale) / 2 - bounds.y * scale,
    scale,
  };
}

export function zoomViewport(view: Viewport, factor: number, center: Point): Viewport {
  const scale = Math.max(MIN_SCALE, Math.min(MAX_SCALE, view.scale * factor));
  const ratio = scale / view.scale;
  return { scale, x: center.x - (center.x - view.x) * ratio, y: center.y - (center.y - view.y) * ratio };
}

export function fitWidth(bounds: Rect, box: Size, { maxScale }: { maxScale: number }): Viewport {
  const scale = Math.min(maxScale, Math.max(MIN_SCALE, (box.width - 2 * MARGIN) / bounds.width));
  const height = bounds.height * scale;
  return {
    x: (box.width - bounds.width * scale) / 2 - bounds.x * scale,
    y: (height <= box.height - 2 * MARGIN ? (box.height - height) / 2 : MARGIN) - bounds.y * scale,
    scale,
  };
}

export function showRect(rect: Rect, box: Size, scale: number): Viewport {
  const axis = (start: number, length: number, room: number) => (length * scale <= room - 2 * MARGIN ? (room - length * scale) / 2 : MARGIN) - start * scale;
  return { x: axis(rect.x, rect.width, box.width), y: axis(rect.y, rect.height, box.height), scale };
}

export function revealRect(view: Viewport, rect: Rect, box: Size): Viewport {
  const axis = (offset: number, start: number, length: number, room: number) => {
    const from = start * view.scale + offset;
    const to = from + length * view.scale;
    if (from < MARGIN || to - from > room - 2 * MARGIN) return offset + MARGIN - from;
    return to > room - MARGIN ? offset + room - MARGIN - to : offset;
  };
  const x = axis(view.x, rect.x, rect.width, box.width);
  const y = axis(view.y, rect.y, rect.height, box.height);
  return x === view.x && y === view.y ? view : { ...view, x, y };
}

export function followBox(view: Viewport, wasWidth: number, box: Size, focus: Rect | undefined): Viewport {
  const kept = wasWidth === box.width ? view : { ...view, x: view.x + (box.width - wasWidth) / 2 };
  return focus ? revealRect(kept, focus, box) : kept;
}
