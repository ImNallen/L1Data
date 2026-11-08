import { create } from "zustand";
import { commands, type DatabaseRef, type MapCursor, type MapError, type MapQuery, type MapQueryJoin, type MapOccurrence } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { dbKey, sameDb } from "../../state/dbKey";
import { useLive, wantedCatalogs } from "../../state/live";
import { activeId, activeTab, tabTarget, useWorkspace } from "../../state/workspace";
import { clearMapHighlights, editorContextOf, highlightMapSource, onEditorContextChanged, sameEditorContext, type EditorContext } from "../editor/editorStates";
import { EDGE_LIMIT, emptyGraph, mergeNeighborhood, type Graph, type Page, type Point, type Viewport } from "./graph";

export type Selection =
  | { kind: "node"; oid: number }
  | { kind: "foreignKey"; id: number }
  | { kind: "join"; index: number }
  | { kind: "occurrence"; id: number }
  | { kind: "none" };
type Analysis =
  | { kind: "off" }
  | { kind: "waiting"; message: string }
  | { kind: "loading"; context: EditorContext; ticket: number }
  | { kind: "ready"; context: EditorContext; query: MapQuery }
  | { kind: "error"; message: string };
export type Exploring = {
  kind: "exploring";
  generation: number;
  db: DatabaseRef;
  root: number;
  version: number;
  graph: Graph;
  selection: Selection;
  analysis: Analysis;
  limit: boolean;
};
export type MapState = { kind: "closed" } | { kind: "unavailable"; db: DatabaseRef; root: number; message: string } | Exploring;

// The viewport sits beside the map, not in it, so a pan or zoom re-renders only the
// transform layer and the zoom readout, never the cards, edges, or details.
export const useDatabaseMap = create<{ map: MapState; viewport: Viewport }>(() => ({ map: { kind: "closed" }, viewport: { x: 80, y: 100, scale: 1 } }));
let generation = 0;
let queryTicket = 0;
let highlighted: string | null = null;
let activeRequests = 0;
let queue: { generation: number; db: DatabaseRef; version: number; oid: number; after: MapCursor | null }[] = [];
let analysisTimer: ReturnType<typeof setTimeout> | undefined;

function clearHighlights(): void {
  if (highlighted) clearMapHighlights(highlighted);
  highlighted = null;
}

function versionFor(db: DatabaseRef): number | undefined {
  return wantedCatalogs(useLive.getState().connections).get(dbKey(db))?.version;
}

export function mapErrorText(error: MapError): string {
  switch (error.kind) {
    case "Gone":
      return "This table is gone or its metadata is no longer visible. Refresh the catalog.";
    case "Stale":
      return "The catalog changed. Reload the map to explore its current relationships.";
    case "Failed":
      return error.message;
    case "Catalog":
      return "Database metadata is unavailable. Connect and refresh the catalog.";
  }
}

export function openDatabaseMap(db: DatabaseRef, root: number): void {
  clearHighlights();
  generation += 1;
  queryTicket += 1;
  queue = [];
  const version = versionFor(db);
  const catalog = viewFor(useCatalog.getState().views, db)?.catalog;
  if (version === undefined || catalog?.version !== version) {
    useDatabaseMap.setState({ map: { kind: "unavailable", db, root, message: "Connect and load this database's catalog before opening its map." } });
    return;
  }
  useDatabaseMap.setState({
    map: {
      kind: "exploring",
      generation,
      db,
      root,
      version,
      graph: emptyGraph(),
      selection: { kind: "none" },
      analysis: { kind: "off" },
      limit: false,
    },
    viewport: { x: 80, y: 100, scale: 1 },
  });
  expandMapNode(root);
}

export function closeDatabaseMap(): void {
  generation += 1;
  queryTicket += 1;
  queue = [];
  clearTimeout(analysisTimer);
  clearHighlights();
  useDatabaseMap.setState({ map: { kind: "closed" } });
}

function invalidate(message: string): void {
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring") return;
  generation += 1;
  queryTicket += 1;
  queue = [];
  clearHighlights();
  useDatabaseMap.setState({ map: { kind: "unavailable", db: map.db, root: map.root, message } });
}

function current(ticket: { generation: number; version: number }): Exploring | null {
  const map = useDatabaseMap.getState().map;
  return map.kind === "exploring" && map.generation === ticket.generation && map.version === ticket.version && versionFor(map.db) === map.version ? map : null;
}

function putPage(map: Exploring, oid: number, page: Page): void {
  const node = map.graph.nodes.get(oid);
  if (!node) return;
  const nodes = new Map(map.graph.nodes);
  nodes.set(oid, { ...node, page });
  useDatabaseMap.setState({ map: { ...map, graph: { ...map.graph, nodes } } });
}

export function expandMapNode(oid: number): void {
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring" || map.limit) return;
  const node = map.graph.nodes.get(oid);
  if (node?.page.kind === "complete" || node?.page.kind === "loading") return;
  if (queue.some((q) => q.generation === map.generation && q.oid === oid)) return;
  const after = node?.page.kind === "more" || node?.page.kind === "error" ? node.page.after : null;
  if (node) putPage(map, oid, { kind: "loading" });
  queue.push({ generation: map.generation, db: map.db, version: map.version, oid, after });
  pump();
}

function pump(): void {
  while (activeRequests < 2 && queue.length) {
    const request = queue.shift();
    if (!request || !current(request)) continue;
    activeRequests += 1;
    void (async () => {
      try {
        const response = await commands.databaseMapNeighborhood(request.db, request.oid, request.version, request.after);
        const map = current(request);
        if (!map) return;
        if (response.status === "error") {
          if (response.error.kind === "Stale") invalidate(mapErrorText(response.error));
          else if (!map.graph.nodes.has(request.oid)) invalidate(mapErrorText(response.error));
          else putPage(map, request.oid, { kind: "error", message: mapErrorText(response.error), after: request.after });
          return;
        }
        if (!sameDb(response.data.db, map.db) || response.data.catalogVersion !== map.version || response.data.root.oid !== request.oid) {
          invalidate(mapErrorText({ kind: "Stale" }));
          return;
        }
        const merged = mergeNeighborhood(map.graph, response.data, request.oid);
        if (merged.kind === "limit") {
          putPage(map, request.oid, request.after ? { kind: "more", after: request.after } : { kind: "unloaded" });
          const held = useDatabaseMap.getState().map;
          if (held.kind === "exploring") useDatabaseMap.setState({ map: { ...held, limit: true } });
        } else {
          useDatabaseMap.setState({ map: { ...map, graph: merged.graph } });
        }
      } catch (error) {
        const map = current(request);
        if (map) {
          const message = error instanceof Error ? error.message : String(error);
          if (map.graph.nodes.has(request.oid)) putPage(map, request.oid, { kind: "error", message, after: request.after });
          else invalidate(message);
        }
      } finally {
        activeRequests -= 1;
        pump();
      }
    })();
  }
}

export function setMapViewport(viewport: Viewport): void {
  const map = useDatabaseMap.getState().map;
  if (map.kind === "exploring") useDatabaseMap.setState({ viewport });
}

export function moveMapNode(oid: number, position: Point): void {
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring") return;
  const node = map.graph.nodes.get(oid);
  if (!node) return;
  const nodes = new Map(map.graph.nodes);
  nodes.set(oid, { ...node, position });
  useDatabaseMap.setState({ map: { ...map, graph: { ...map.graph, nodes } } });
}

function contextFor(map: Exploring): EditorContext | null {
  const tab = activeTab(useWorkspace.getState());
  return tab && sameDb(tabTarget(tab), map.db) ? editorContextOf(tab.id) : null;
}

export function occurrenceOid(occurrence: MapOccurrence): number | null {
  return occurrence.resolution.kind === "Unresolved" ? null : occurrence.resolution.oid;
}

function selectionRanges(map: Exploring, query: MapQuery): MapOccurrence["range"][] {
  const selection = map.selection;
  switch (selection.kind) {
    case "node":
      return query.occurrences.filter((o) => occurrenceOid(o) === selection.oid).map((o) => o.range);
    case "join": {
      const edge = query.joins[selection.index];
      return edge ? [edge.range] : [];
    }
    case "occurrence": {
      const occurrence = query.occurrences.find((o) => o.id === selection.id);
      return occurrence ? [occurrence.range] : [];
    }
    case "foreignKey":
    case "none":
      return [];
  }
}

export function selectMap(selection: Selection): void {
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring") return;
  const next = { ...map, selection };
  useDatabaseMap.setState({ map: next });
  clearHighlights();
  const analysis = next.analysis;
  const context = contextFor(next);
  if (analysis.kind === "ready" && context && sameEditorContext(context, analysis.context) && versionFor(next.db) === next.version) {
    if (highlightMapSource(analysis.context, selectionRanges(next, analysis.query))) highlighted = analysis.context.tabId;
  }
}

export function toggleMapOverlay(): void {
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring") return;
  clearHighlights();
  queryTicket += 1;
  useDatabaseMap.setState({
    map: { ...map, analysis: map.analysis.kind !== "off" ? { kind: "off" } : { kind: "waiting", message: "Reading cursor statement…" } },
  });
  scheduleAnalysis();
}

function scheduleAnalysis(): void {
  clearTimeout(analysisTimer);
  clearHighlights();
  queryTicket += 1;
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring" || map.analysis.kind === "off") return;
  const context = contextFor(map);
  if (!context) {
    useDatabaseMap.setState({
      map: { ...map, analysis: { kind: "waiting", message: "Activate a SQL tab in this database to overlay its cursor statement." } },
    });
    return;
  }
  const ticket = queryTicket;
  useDatabaseMap.setState({ map: { ...map, analysis: { kind: "loading", context, ticket } } });
  analysisTimer = setTimeout(() => {
    void analyze(map, context, ticket);
  }, 180);
}

async function analyze(captured: Exploring, context: EditorContext, ticket: number): Promise<void> {
  try {
    const response = await commands.databaseMapQuery(captured.db, context.doc.toString(), context.selection.main.head, captured.version);
    const map = current(captured);
    const now = map ? contextFor(map) : null;
    if (!map || map.analysis.kind === "off" || ticket !== queryTicket || !now || !sameEditorContext(now, context)) return;
    if (response.status === "error") {
      if (response.error.kind === "Stale") invalidate(mapErrorText(response.error));
      else useDatabaseMap.setState({ map: { ...map, analysis: { kind: "error", message: mapErrorText(response.error) } } });
    } else if (sameDb(response.data.db, map.db) && response.data.catalogVersion === map.version) {
      useDatabaseMap.setState({ map: { ...map, analysis: { kind: "ready", context, query: response.data } } });
    } else {
      invalidate(mapErrorText({ kind: "Stale" }));
    }
  } catch (error) {
    const map = current(captured);
    if (map && ticket === queryTicket)
      useDatabaseMap.setState({ map: { ...map, analysis: { kind: "error", message: error instanceof Error ? error.message : String(error) } } });
  }
}

export function startMapLifecycle(): () => void {
  const offEditor = onEditorContextChanged(scheduleAnalysis);
  const offWorkspace = useWorkspace.subscribe((next, prev) => {
    const before = activeTab(prev);
    const after = activeTab(next);
    if (activeId(next) !== activeId(prev) || !sameDb(tabTarget(before), tabTarget(after))) scheduleAnalysis();
  });
  const offLive = useLive.subscribe(() => {
    const map = useDatabaseMap.getState().map;
    if (map.kind === "exploring" && versionFor(map.db) !== map.version)
      invalidate("This database disconnected or its catalog changed. Reload the map after reconnecting or refreshing.");
  });
  return () => {
    offEditor();
    offWorkspace();
    offLive();
    clearTimeout(analysisTimer);
    clearHighlights();
  };
}

export function queryJoinEndpoints(query: MapQuery, join: MapQueryJoin): [number, number] | null {
  const source = query.occurrences.find((o) => o.id === join.source);
  const target = query.occurrences.find((o) => o.id === join.target);
  const a = source ? occurrenceOid(source) : null;
  const b = target ? occurrenceOid(target) : null;
  return a !== null && b !== null ? [a, b] : null;
}

export function displayedQueryJoins(map: Exploring): MapQueryJoin[] {
  return map.analysis.kind === "ready" ? map.analysis.query.joins.slice(0, Math.max(0, EDGE_LIMIT - map.graph.edges.size)) : [];
}
