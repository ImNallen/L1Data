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
