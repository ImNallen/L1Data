import { expect, it } from "vitest";
import { fitViewport, fitWidth, followBox, revealRect, showRect, zoomViewport, type Rect, type Viewport } from "./viewport";

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

it("fits a tall map to the box's width and shows it from the top", () => {
  const bounds = { x: 0, y: 40, width: 1856, height: 2000 };
  const view = fitWidth(bounds, { width: 1000, height: 600 }, { maxScale: 1 });
  expect(view.scale).toBe(0.5);
  expect(onScreen(view, bounds)).toEqual({ x: 36, y: 36, width: 928, height: 1000 });
});

it("fits a short map to the width and centers it down the box, never above the maximum scale", () => {
  const bounds = { x: 10, y: 10, width: 400, height: 100 };
  const view = fitWidth(bounds, { width: 1000, height: 600 }, { maxScale: 1 });
  expect(view.scale).toBe(1);
  expect(onScreen(view, bounds)).toEqual({ x: 300, y: 250, width: 400, height: 100 });
});

it("shows a map at 100%, centered across when it fits and from the top margin when it is taller than the box", () => {
  const bounds = { x: 20, y: 0, width: 800, height: 900 };
  const view = showRect(bounds, { width: 1000, height: 600 }, 1);
  expect(onScreen(view, bounds)).toEqual({ x: 100, y: 36, width: 800, height: 900 });
});

it("reveals a card past the right edge by the least pan, and leaves a visible one in place", () => {
  const view = { x: 0, y: 0, scale: 1 };
  const box = { width: 1000, height: 600 };
  const card = { x: 900, y: 100, width: 196, height: 120 };
  expect(onScreen(revealRect(view, card, box), card)).toEqual({ x: 768, y: 100, width: 196, height: 120 });
  const visible = { x: 100, y: 100, width: 196, height: 120 };
  expect(revealRect(view, visible, box)).toBe(view);
});

it("reveals a card above the top edge at the margin", () => {
  const view = { x: 0, y: -300, scale: 0.5 };
  const card = { x: 100, y: 200, width: 196, height: 120 };
  expect(onScreen(revealRect(view, card, { width: 1000, height: 600 }), card).y).toBe(36);
});

it("keeps the center when the box narrows, then shows the whole focused card in the narrower box", () => {
  const view = { x: 0, y: 0, scale: 1 };
  const box = { width: 888, height: 600 };
  expect(followBox(view, 1200, box, undefined)).toEqual({ x: -156, y: 0, scale: 1 });
  const card = { x: 40, y: 100, width: 196, height: 120 };
  expect(onScreen(followBox(view, 1200, box, card), card)).toEqual({ x: 36, y: 100, width: 196, height: 120 });
});
