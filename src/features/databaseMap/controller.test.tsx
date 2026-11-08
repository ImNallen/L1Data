// @vitest-environment jsdom
import { EditorView } from "@codemirror/view";
import { fireEvent, render, screen, cleanup, act } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { commands, type MapNeighborhood, type MapQuery, type MapRelation } from "../../ipc/bindings";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectedState, databaseState } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { activateEditor, dropEditor, editorContextOf, editorStateOf, moveCursor, replaceDoc, seedEditor } from "../editor/editorStates";
import { DatabaseMap } from "./DatabaseMap";
import { closeDatabaseMap, expandMapNode, openDatabaseMap, selectMap, startMapLifecycle, toggleMapOverlay, useDatabaseMap, type Exploring } from "./controller";
import { CARD_HEIGHT, CARD_WIDTH, EDGE_LIMIT, NODE_LIMIT, emptyGraph, fitViewport, mergeNeighborhood } from "./graph";
import { cellsState, moveCell } from "../../testing/notebooks";

const db = { connectionId: "map-test", database: "shop" };
const sql = "SELECT * FROM public.orders a JOIN public.people b ON a.id=b.id";
const relation = (oid: number): MapRelation => ({ oid, schema: "public", name: `table_${oid}`, columns: ["id"] });
const page = (oid: number, targets: number[] = []): MapNeighborhood => ({
  db,
  catalogVersion: 1,
  root: relation(oid),
  relations: targets.map(relation),
  after: null,
  foreignKeys: targets.map((target) => ({
    id: oid * 1000 + target,
    name: `fk_${oid}_${target}`,
    source: oid,
    target,
    columns: [{ source: "id", target: "id" }],
    enabled: true,
    validated: true,
  })),
});
const query = (): MapQuery => ({
  db,
  catalogVersion: 1,
  statement: { from: 0, to: sql.length },
  incomplete: false,
  occurrences: [
    { id: 0, name: "public.orders", alias: "a", range: { from: 14, to: 27 }, resolution: { kind: "CatalogMatch", oid: 1 } },
    { id: 1, name: "public.people", alias: "b", range: { from: 35, to: 48 }, resolution: { kind: "CatalogMatch", oid: 2 } },
  ],
  joins: [{ source: 0, target: 1, columns: { source: "id", target: "id" }, range: { from: 54, to: sql.length } }],
});
function exploring(): Exploring {
  const map = useDatabaseMap.getState().map;
  if (map.kind !== "exploring") throw new Error(`Expected map, got ${map.kind}`);
  return map;
}
const pending: (() => void)[] = [];
function deferred<T>(fallback: T): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve: (value: T) => void = () => {
    throw new Error("Promise executor did not run");
  };
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  pending.push(() => resolve(fallback));
  return { promise, resolve };
}
const disposers: (() => void)[] = [];
const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
};

function marks(): number[][] {
  const state = editorStateOf("sql");
  const ranges: number[][] = [];
  for (const decoration of state.facet(EditorView.decorations)) {
    if (typeof decoration !== "function")
      decoration.between(0, state.doc.length, (from, to, value) => {
        if (value.spec.class === "cm-map-source") ranges.push([from, to]);
      });
  }
  return ranges;
}

beforeEach(() => {
  useLive.setState({ connections: { [db.connectionId]: connectedState(db.connectionId, [databaseState("shop", true, ["public"])]) } });
  useCatalog.setState({ views: { [dbKey(db)]: buildCatalogView(shopCatalog(db.connectionId, 1)) } });
  useWorkspace.setState(cellsState([{ id: "sql", title: "Query", console: { db, mode: "AutoCommit" }, table: null, file: null }]));
  seedEditor("sql", sql);
  activateEditor("sql");
  vi.spyOn(commands, "databaseMapNeighborhood").mockImplementation(async (_db, oid) => ({ status: "ok", data: page(oid, oid === 1 ? [2, 3, 4] : []) }));
  vi.spyOn(commands, "databaseMapQuery").mockResolvedValue({ status: "ok", data: query() });
});

afterEach(async () => {
  cleanup();
  for (const dispose of disposers.splice(0)) dispose();
  closeDatabaseMap();
  for (const settle of pending.splice(0)) settle();
  await flush();
  dropEditor("sql");
  dropEditor("other");
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState({ views: {} });
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("explores neighbors, preserves dragged positions, and keeps parallel constraints", async () => {
  openDatabaseMap(db, 1);
  await flush();
  const before = exploring().graph.nodes.get(2)?.position;
  const follow = {
    ...page(2, [5]),
    foreignKeys: [
      ...page(2, [5]).foreignKeys,
      { id: 2006, name: "parallel", source: 2, target: 5, columns: [{ source: "other", target: "id" }], enabled: true, validated: true },
    ],
  };
  vi.mocked(commands.databaseMapNeighborhood).mockResolvedValueOnce({ status: "ok", data: follow });
  expandMapNode(2);
  await flush();
  expect(exploring().graph.nodes.has(5)).toBe(true);
  expect(exploring().graph.nodes.get(2)?.position).toEqual(before);
  expect(exploring().graph.edges.size).toBe(5);
  expect(exploring().graph.nodes.get(2)?.page.kind).toBe("complete");
});

it("limits neighborhood concurrency to two and drains queued exploration", async () => {
  openDatabaseMap(db, 1);
  await flush();
  const a = deferred<Awaited<ReturnType<typeof commands.databaseMapNeighborhood>>>({ status: "ok", data: page(2) });
  const b = deferred<Awaited<ReturnType<typeof commands.databaseMapNeighborhood>>>({ status: "ok", data: page(3) });
  vi.mocked(commands.databaseMapNeighborhood).mockReturnValueOnce(a.promise).mockReturnValueOnce(b.promise);
  expandMapNode(2);
  expandMapNode(3);
  expandMapNode(4);
  expandMapNode(2);
  expect(commands.databaseMapNeighborhood).toHaveBeenCalledTimes(3);
  a.resolve({ status: "ok", data: page(2) });
  await flush();
  expect(commands.databaseMapNeighborhood).toHaveBeenCalledTimes(4);
  b.resolve({ status: "ok", data: page(3) });
  await flush();
});

it("rejects late root replies after refocus and close", async () => {
  const late = deferred<Awaited<ReturnType<typeof commands.databaseMapNeighborhood>>>({ status: "ok", data: page(1, [2]) });
  vi.mocked(commands.databaseMapNeighborhood).mockReturnValueOnce(late.promise);
  openDatabaseMap(db, 1);
  openDatabaseMap(db, 9);
  await flush();
  late.resolve({ status: "ok", data: page(1, [2]) });
  await flush();
  expect([...exploring().graph.nodes.keys()]).toEqual([9]);
  closeDatabaseMap();
  expect(useDatabaseMap.getState().map.kind).toBe("closed");
});

it("invalidates graph and late pages when the catalog refreshes or connection disconnects", async () => {
  disposers.push(startMapLifecycle());
  openDatabaseMap(db, 1);
  await flush();
  const late = deferred<Awaited<ReturnType<typeof commands.databaseMapNeighborhood>>>({ status: "ok", data: page(2, [5]) });
  vi.mocked(commands.databaseMapNeighborhood).mockReturnValueOnce(late.promise);
  expandMapNode(2);
  const state = connectedState(db.connectionId, [{ ...databaseState("shop", true, ["public"]), catalog: { kind: "Ready", version: 2 } }]);
  useLive.setState({ connections: { [db.connectionId]: state } });
  late.resolve({ status: "ok", data: page(2, [5]) });
  await flush();
  expect(useDatabaseMap.getState().map.kind).toBe("unavailable");
  useLive.setState({ connections: { [db.connectionId]: connectedState(db.connectionId, [databaseState("shop", true, ["public"])]) } });
  openDatabaseMap(db, 1);
  await flush();
  useLive.setState({ connections: {} });
  expect(useDatabaseMap.getState().map.kind).toBe("unavailable");
});

it("selects source ranges without changing text, selection, target, or executing SQL, and clears on editing", async () => {
  vi.useFakeTimers();
  disposers.push(startMapLifecycle());
  const run = vi.spyOn(commands, "consoleRun");
  openDatabaseMap(db, 1);
  await flush();
  toggleMapOverlay();
  await vi.advanceTimersByTimeAsync(200);
  const before = editorContextOf("sql");
  const target = useWorkspace.getState().tabs.sql?.console;
  selectMap({ kind: "node", oid: 1 });
  expect(marks()).toEqual([[14, 27]]);
  selectMap({ kind: "join", index: 0 });
  expect(marks()).toEqual([[54, sql.length]]);
  expect(editorContextOf("sql")).toEqual(before);
  expect(useWorkspace.getState().tabs.sql?.console).toBe(target);
  expect(run).not.toHaveBeenCalled();
  replaceDoc("sql", "SELECT 1");
  expect(marks()).toEqual([]);
  expect(exploring().analysis.kind).toBe("loading");
  closeDatabaseMap();
  expect(marks()).toEqual([]);
});

it.each(["edit", "cursor", "tab", "database", "root", "disconnect", "catalog", "close"])("rejects query replies after %s changes", async (change) => {
  vi.useFakeTimers();
  disposers.push(startMapLifecycle());
  const late = deferred<Awaited<ReturnType<typeof commands.databaseMapQuery>>>({ status: "ok", data: query() });
  vi.mocked(commands.databaseMapQuery).mockReturnValueOnce(late.promise);
  openDatabaseMap(db, 1);
  await flush();
  toggleMapOverlay();
  await vi.advanceTimersByTimeAsync(200);
  switch (change) {
    case "edit":
      replaceDoc("sql", "SELECT 2");
      break;
    case "cursor":
      moveCursor("sql", 8);
      break;
    case "tab":
      seedEditor("other", sql);
      useWorkspace.getState().addTab({ id: "other", title: "Other", console: { db, mode: "AutoCommit" }, table: null, file: null });
      activateEditor("other");
      break;
    case "database":
      moveCell("sql", { ...db, database: "reporting" });
      break;
    case "root":
      openDatabaseMap(db, 9);
      break;
    case "disconnect":
      useLive.setState({ connections: {} });
      break;
    case "catalog":
      useLive.setState({
        connections: { [db.connectionId]: connectedState(db.connectionId, [{ ...databaseState("shop", true, []), catalog: { kind: "Ready", version: 2 } }]) },
      });
      break;
    case "close":
      closeDatabaseMap();
      break;
  }
  late.resolve({ status: "ok", data: query() });
  await flush();
  const map = useDatabaseMap.getState().map;
  expect(map.kind === "exploring" && map.analysis.kind === "ready").toBe(false);
  expect(marks()).toEqual([]);
});

it("keeps the continuation cursor until More succeeds", async () => {
  const first = page(1, [2]);
  const cursor = { db, root: 1, catalogVersion: 1, lastConstraint: 1002 };
  first.after = cursor;
  vi.mocked(commands.databaseMapNeighborhood).mockResolvedValueOnce({ status: "ok", data: first });
  openDatabaseMap(db, 1);
  await flush();
  expect(exploring().graph.nodes.get(1)?.page.kind).toBe("more");
  vi.mocked(commands.databaseMapNeighborhood).mockResolvedValueOnce({ status: "error", error: { kind: "Failed", message: "Temporary metadata failure" } });
  expandMapNode(1);
  await flush();
  expect(exploring().graph.nodes.get(1)?.page).toEqual({ kind: "error", message: "Temporary metadata failure", after: cursor });
  vi.mocked(commands.databaseMapNeighborhood).mockResolvedValueOnce({ status: "ok", data: page(1, [3]) });
  expandMapNode(1);
  await flush();
  expect(commands.databaseMapNeighborhood).toHaveBeenLastCalledWith(db, 1, 1, cursor);
  expect(exploring().graph.nodes.has(2) && exploring().graph.nodes.has(3)).toBe(true);
  expect(exploring().graph.nodes.get(1)?.page.kind).toBe("complete");
});

it("exposes a graph limit without consuming exploration and permits a new focus", async () => {
  vi.mocked(commands.databaseMapNeighborhood).mockImplementation(async (_db, oid) => ({
    status: "ok",
    data: page(oid, oid === 1 ? Array.from({ length: 30 }, (_, i) => i + 2) : Array.from({ length: 30 }, (_, i) => 100 + (oid - 2) * 30 + i)),
  }));
  openDatabaseMap(db, 1);
  await flush();
  for (const oid of [2, 3, 4, 5]) {
    expandMapNode(oid);
    await flush();
  }
  const size = exploring().graph.nodes.size;
  expandMapNode(6);
  await flush();
  expect(exploring().limit).toBe(true);
  expect(exploring().graph.nodes.size).toBe(size);
  expect(exploring().graph.nodes.get(6)?.page.kind).toBe("unloaded");
  openDatabaseMap(db, 6);
  await flush();
  expect(exploring().limit).toBe(false);
  expect(exploring().root).toBe(6);
});

it.each(["cursor", "tab", "database", "disconnect", "catalog", "overlay", "close"])("clears existing source decorations after %s changes", async (change) => {
  vi.useFakeTimers();
  disposers.push(startMapLifecycle());
  openDatabaseMap(db, 1);
  await flush();
  toggleMapOverlay();
  await vi.advanceTimersByTimeAsync(200);
  selectMap({ kind: "node", oid: 1 });
  expect(marks()).toEqual([[14, 27]]);
  switch (change) {
    case "cursor":
      moveCursor("sql", 7);
      break;
    case "tab":
      useWorkspace.getState().showDatabase({ ...db, database: "reporting" });
      break;
    case "database":
      moveCell("sql", { ...db, database: "reporting" });
      break;
    case "disconnect":
      useLive.setState({ connections: {} });
      break;
    case "catalog":
      useLive.setState({
        connections: { [db.connectionId]: connectedState(db.connectionId, [{ ...databaseState("shop", true, []), catalog: { kind: "Ready", version: 2 } }]) },
      });
      break;
    case "overlay":
      toggleMapOverlay();
      break;
    case "close":
      closeDatabaseMap();
      break;
  }
  expect(marks()).toEqual([]);
});

it("does not silently consume a page beyond graph limits", () => {
  const large = page(
    1,
    Array.from({ length: NODE_LIMIT }, (_, i) => i + 2),
  );
  expect(mergeNeighborhood(emptyGraph(), large, 1).kind).toBe("limit");
  const edgePage = page(1, [2]);
  edgePage.foreignKeys = Array.from({ length: EDGE_LIMIT + 1 }, (_, id) => ({
    id,
    name: `key${id}`,
    source: 1,
    target: 2,
    columns: [],
    enabled: true,
    validated: true,
  }));
  expect(mergeNeighborhood(emptyGraph(), edgePage, 1).kind).toBe("limit");
});

it("fits every card in a large neighborhood inside the viewport", () => {
  const result = mergeNeighborhood(
    emptyGraph(),
    page(
      1,
      Array.from({ length: NODE_LIMIT - 1 }, (_, i) => i + 2),
    ),
    1,
  );
  if (result.kind !== "merged") throw new Error("Neighborhood should fit the graph limit");
  const viewport = fitViewport(result.graph, 1000, 600);
  for (const node of result.graph.nodes.values()) {
    expect(node.position.x * viewport.scale + viewport.x).toBeGreaterThanOrEqual(0);
    expect(node.position.y * viewport.scale + viewport.y).toBeGreaterThanOrEqual(0);
    expect((node.position.x + CARD_WIDTH) * viewport.scale + viewport.x).toBeLessThanOrEqual(1000);
    expect((node.position.y + CARD_HEIGHT) * viewport.scale + viewport.y).toBeLessThanOrEqual(600);
  }
});

it("renders exploration, ordered declared key details, zoom, overlay, and close controls", async () => {
  openDatabaseMap(db, 1);
  await flush();
  render(<DatabaseMap />);
  expect(screen.getByTestId("map-node-2")).toBeTruthy();
  fireEvent.click(screen.getByTestId("map-fk-1002"));
  expect(screen.getByTestId("map-fk-detail").textContent).toContain("id → id");
  const nodePosition = exploring().graph.nodes.get(2)?.position;
  fireEvent.keyDown(screen.getByTestId("map-select-2"), { key: "ArrowLeft" });
  expect(exploring().graph.nodes.get(2)?.position.x).toBe((nodePosition?.x ?? 0) - 24);
  expect(screen.getByTestId("map-status").textContent).toBe("Selected foreign key fk_1_2.");
  expect(screen.getByTestId("map-details").getAttribute("aria-live")).toBeNull();
  const before = useDatabaseMap.getState().viewport.scale;
  const explored = exploring();
  fireEvent.click(screen.getByTestId("map-zoom-in"));
  expect(useDatabaseMap.getState().viewport.scale).toBeGreaterThan(before);
  // A zoom moves only the viewport, so cards and edges keep their props and skip rendering.
  expect(exploring()).toBe(explored);
  await act(async () => {
    fireEvent.click(screen.getByTestId("map-expand-2"));
    await flush();
  });
  expect(screen.getByTestId("map-expand-2").textContent).toBe("Explored");
  fireEvent.click(screen.getByTestId("map-overlay"));
  expect(screen.getByTestId("map-overlay").getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(screen.getByTestId("map-close"));
  expect(screen.queryByTestId("database-map")).toBeNull();
});

it("bounds declared and SQL relationships together and reports hidden predicates", async () => {
  vi.useFakeTimers();
  const analysis = query();
  analysis.joins = Array.from({ length: EDGE_LIMIT + 1 }, () => query().joins).flat();
  vi.mocked(commands.databaseMapQuery).mockResolvedValue({ status: "ok", data: analysis });
  openDatabaseMap(db, 1);
  await flush();
  render(<DatabaseMap />);
  toggleMapOverlay();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(200);
  });
  expect(document.querySelectorAll('[data-testid^="map-fk-"], [data-testid^="map-join-"]')).toHaveLength(EDGE_LIMIT);
  expect(screen.getByTestId("map-query-limit").textContent).toContain("4 SQL equalities are hidden");
});
