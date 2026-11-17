import { create } from "zustand";
import type { TabId } from "../../ipc/bindings";
import type { TableKey } from "./model";
import type { Point } from "./viewport";

export type MapCellView = {
  focus: TableKey | null;
  find: string;
  moved: ReadonlyMap<TableKey, Point>;
};

export const NO_VIEW: MapCellView = { focus: null, find: "", moved: new Map() };

export const useMapViews = create<{ views: Record<TabId, MapCellView> }>(() => ({ views: {} }));

function patch(id: TabId, change: (view: MapCellView) => Partial<MapCellView>): void {
  useMapViews.setState((s) => {
    const view = s.views[id] ?? NO_VIEW;
    return { views: { ...s.views, [id]: { ...view, ...change(view) } } };
  });
}

export function useMapView(id: TabId): MapCellView {
  return useMapViews((s) => s.views[id] ?? NO_VIEW);
}

export function focusTable(id: TabId, key: TableKey | null): void {
  patch(id, () => ({ focus: key }));
}

export function clearFocusIfAny(id: TabId): boolean {
  if (!useMapViews.getState().views[id]?.focus) return false;
  focusTable(id, null);
  return true;
}

export function setFind(id: TabId, find: string): void {
  patch(id, () => ({ find }));
}

export function moveCard(id: TabId, key: TableKey, to: Point): void {
  patch(id, (view) => ({ moved: new Map(view.moved).set(key, to) }));
}

export function forgetMovedCards(id: TabId): void {
  patch(id, () => ({ moved: new Map() }));
}

export function forgetMapView(id: TabId): void {
  useMapViews.setState((s) => {
    if (!s.views[id]) return {};
    const { [id]: _, ...views } = s.views;
    return { views };
  });
}
