import { useEffect, useMemo } from "react";
import type { DatabaseRef, MapColumns, TabId } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { servedCatalogVersion, useLive } from "../../state/live";
import { isMapCell, useWorkspace, type MapTab } from "../../state/workspace";
import { refreshCatalog } from "../connections/connectionActions";
import { setTabSchema } from "../editor/runActions";
import { mapKey, useSchemaMapData } from "./data";
import { layoutMap, routeLinks, withMoves, type CardMetrics, type LinkRoute, type MapLayout } from "./layout";
import { findMatches, focusOf, type ErModel, type Focus, type Oid, type TableKey } from "./model";
import { focusTable, forgetMovedCards, moveCard, setFind, useMapView, type MapCellView } from "./views";
import type { Point } from "./viewport";

export const CARD_METRICS: CardMetrics = { width: 196, header: 32, row: 22, bottomPadding: 6, layerGap: 72, cardGap: 20, componentGap: 56, frameInset: 20 };

export type MapStatus =
  | { kind: "unconnected" }
  | { kind: "loading" }
  | { kind: "ready"; fetchedAtMs: number | null }
  | { kind: "refreshing"; fetchedAtMs: number | null }
  | { kind: "error"; message: string };

export type SchemaMapView = {
  tab: MapTab;
  db: DatabaseRef | null;
  schema: string | null;
  schemas: readonly string[];
  columns: MapColumns;
  status: MapStatus;
  model: ErModel | null;
  layout: MapLayout | null;
  routes: readonly LinkRoute[];
  view: MapCellView;
  focus: Focus | null;
  matches: ReadonlySet<Oid> | null;
  actions: {
    focus(key: TableKey | null): void;
    find(text: string): void;
    move(key: TableKey, to: Point): void;
    autoLayout(): void;
    setColumns(columns: MapColumns): void;
    setSchema(schema: string): void;
    refresh(): void;
  };
};

const layouts = new WeakMap<ErModel, Partial<Record<MapColumns, MapLayout>>>();

function layoutFor(model: ErModel, columns: MapColumns): MapLayout {
  const held = layouts.get(model) ?? {};
  layouts.set(model, held);
  return (held[columns] ??= layoutMap(model, columns, CARD_METRICS));
}

const NO_SCHEMAS: readonly string[] = [];
const NO_ROUTES: readonly LinkRoute[] = [];

export function useSchemaMap(id: TabId): SchemaMapView | null {
  const tab = useWorkspace((s) => s.tabs[id]);
  const mapTab = isMapCell(tab) ? tab : null;
  const db = mapTab?.console.db ?? null;
  const asked = mapTab?.console.schema ?? null;
  const version = useLive((s) => (db ? servedCatalogVersion(s.connections, db) : undefined));
  const catalog = useCatalog((s) => viewFor(s.views, db)?.catalog);
  const entry = useSchemaMapData((s) => (db ? s.entries[mapKey(db, asked)] : undefined));
  const view = useMapView(id);
  const expanded = mapTab !== null && !mapTab.collapsed;

  useEffect(() => {
    if (db && version !== undefined && expanded) void useSchemaMapData.getState().ensure(db, asked, version);
  }, [db, asked, version, expanded]);

  const model = entry?.lastModel ?? null;
  const columns = mapTab?.map.columns ?? "All";
  const layout = useMemo(() => {
    if (!model) return null;
    const auto = layoutFor(model, columns);
    if (!view.moved.size) return auto;
    const moved = new Map<Oid, Point>();
    for (const [key, at] of view.moved) {
      const oid = model.byKey.get(key);
      if (oid !== undefined) moved.set(oid, at);
    }
    return withMoves(auto, moved);
  }, [model, columns, view.moved]);
  const routes = useMemo(() => (model && layout ? routeLinks(model, layout, columns) : NO_ROUTES), [model, layout, columns]);
  const focusOid = view.focus ? model?.byKey.get(view.focus) : undefined;
  const focus = useMemo(() => (model && focusOid !== undefined ? focusOf(model, focusOid) : null), [model, focusOid]);
  const matches = useMemo(() => (model ? findMatches(model, view.find) : null), [model, view.find]);
  const schemas = useMemo(() => catalog?.schemas.map((s) => s.name) ?? NO_SCHEMAS, [catalog]);

  if (!mapTab) return null;
  const status: MapStatus =
    !db || (!entry && version === undefined)
      ? { kind: "unconnected" }
      : !entry || (entry.status === "loading" && !model)
        ? { kind: "loading" }
        : entry.status === "error"
          ? { kind: "error", message: entry.error ?? "The map could not load." }
          : entry.status === "loading"
            ? { kind: "refreshing", fetchedAtMs: entry.fetchedAtMs }
            : { kind: "ready", fetchedAtMs: entry.fetchedAtMs };
  return {
    tab: mapTab,
    db,
    schema: model?.schema ?? asked ?? catalog?.defaultSchema ?? null,
    schemas,
    columns,
    status,
    model,
    layout,
    routes,
    view,
    focus,
    matches,
    actions: {
      focus: (key) => focusTable(id, key),
      find: (text) => setFind(id, text),
      move: (key, to) => moveCard(id, key, to),
      autoLayout: () => forgetMovedCards(id),
      setColumns: (next) => useWorkspace.getState().setMapColumns(id, next),
      setSchema: (schema) => void setTabSchema(id, schema),
      refresh: () => {
        if (!db) return;
        if (entry?.status === "error" && version !== undefined) void useSchemaMapData.getState().ensure(db, asked, version);
        void refreshCatalog(db);
      },
    },
  };
}
