import { expect, it } from "vitest";
import { fitViewport, zoomViewport, type Rect, type Viewport } from "./viewport";

const onScreen = (view: Viewport, r: Rect) => ({
  x: r.x * view.scale + view.x,
  y: r.y * view.scale + view.y,
  width: r.width * view.scale,
  height: r.height * view.scale,
});

it("centers bounds that fit at the maximum scale", () => {
  const bounds = { x: 100, y: 50, width: 400, height: 200 };
  const view = fitViewport(bounds, { width: 1000, height: 600 }, { maxScale: 1 });
  expect(view.scale).toBe(1);
  expect(onScreen(view, bounds)).toEqual({ x: 300, y: 200, width: 400, height: 200 });
});

it("shrinks wide bounds to the box's width inside a 36 px margin", () => {
  const bounds = { x: -200, y: 0, width: 1856, height: 300 };
  const view = fitViewport(bounds, { width: 1000, height: 600 }, { maxScale: 1 });
  expect(view.scale).toBe(0.5);
  expect(onScreen(view, bounds)).toEqual({ x: 36, y: 225, width: 928, height: 150 });
});

it("zooms about a screen point, which stays over the same layout point", () => {
  const view = { x: 40, y: -20, scale: 0.5 };
  const zoomed = zoomViewport(view, 2, { x: 300, y: 200 });
  expect(zoomed.scale).toBe(1);
  const layoutUnder = (v: Viewport) => ({ x: (300 - v.x) / v.scale, y: (200 - v.y) / v.scale });
  expect(layoutUnder(zoomed)).toEqual(layoutUnder(view));
});

it("caps zoom at 2.5", () => {
  expect(zoomViewport({ x: 0, y: 0, scale: 2 }, 4, { x: 0, y: 0 }).scale).toBe(2.5);
});
