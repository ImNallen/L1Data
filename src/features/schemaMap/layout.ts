import { visibleColumns, type ErModel, type ErTable, type MapColumns, type Oid } from "./model";
import type { Point, Rect } from "./viewport";

export type CardMetrics = {
  width: number;
  header: number;
  row: number;
  bottomPadding: number;
  layerGap: number;
  cardGap: number;
  componentGap: number;
  frameInset: number;
};

export type LinkBend = { ends: readonly [Oid, Oid]; points: readonly Point[] };

export type MapLayout = {
  metrics: CardMetrics;
  cards: ReadonlyMap<Oid, Rect>;
  bends: ReadonlyMap<string, LinkBend>;
  unlinkedFrame: Rect | null;
  bounds: Rect;
};

const MAX_BEND_DUMMIES = 20_000;
const ASPECT = 16 / 9;

type Placed = { width: number; height: number; cards: Map<Oid, Rect>; bends: Map<string, LinkBend> };

export function layoutMap(model: ErModel, columns: MapColumns, metrics: CardMetrics): MapLayout {
  const height = (t: ErTable) => metrics.header + visibleColumns(t, columns).length * metrics.row + metrics.bottomPadding;
  const rank = new Map([...model.tables.keys()].map((oid, i) => [oid, i]));
  const placed = components(model).map((members) => layoutComponent(model, members, rank, height, metrics));
  const isolated = model.isolated.map((oid) => model.tables.get(oid)!);

  const area =
    placed.reduce((sum, c) => sum + c.width * c.height, 0) +
    isolated.reduce((sum, t) => sum + (metrics.width + metrics.cardGap) * (height(t) + metrics.cardGap), 0);
  const shelfWidth = Math.max(...placed.map((c) => c.width), Math.sqrt(area * ASPECT));

  const cards = new Map<Oid, Rect>();
  const bends = new Map<string, LinkBend>();
  let x = 0;
  let y = 0;
  let shelfHeight = 0;
  for (const c of placed) {
    if (x > 0 && x + c.width > shelfWidth) {
      x = 0;
      y += shelfHeight + metrics.componentGap;
      shelfHeight = 0;
    }
    for (const [oid, r] of c.cards) cards.set(oid, { ...r, x: r.x + x, y: r.y + y });
    for (const [id, b] of c.bends) bends.set(id, { ends: b.ends, points: b.points.map((p) => ({ x: p.x + x, y: p.y + y })) });
    x += c.width + metrics.componentGap;
    shelfHeight = Math.max(shelfHeight, c.height);
  }

  const frame = isolated.length ? isolatedGrid(isolated, placed.length ? y + shelfHeight + metrics.componentGap : 0, shelfWidth, height, metrics, cards) : null;
  return { metrics, cards, bends, unlinkedFrame: frame, bounds: boundsOf(cards, bends, frame) };
}

function components(model: ErModel): Oid[][] {
  const isolated = new Set(model.isolated);
  const seen = new Set<Oid>();
  const found: Oid[][] = [];
  for (const start of model.tables.keys()) {
    if (isolated.has(start) || seen.has(start)) continue;
    const members = [start];
    seen.add(start);
    for (let i = 0; i < members.length; i++) {
      for (const id of model.incident.get(members[i]!)!) {
        const link = model.links.get(id)!;
        for (const end of [link.source, link.target]) {
          if (!seen.has(end)) {
            seen.add(end);
            members.push(end);
          }
        }
      }
    }
    found.push(members);
  }
  return found.sort((a, b) => b.length - a.length);
}

function layoutComponent(model: ErModel, members: Oid[], rank: ReadonlyMap<Oid, number>, height: (t: ErTable) => number, m: CardMetrics): Placed {
  const byRank = (a: Oid, b: Oid) => rank.get(a)! - rank.get(b)!;
  members.sort(byRank);

  const out = new Map<Oid, Oid[]>(members.map((oid) => [oid, []]));
  for (const oid of members) {
    for (const id of model.incident.get(oid)!) {
      const link = model.links.get(id)!;
      if (link.source === oid && link.target !== oid && !out.get(oid)!.includes(link.target)) out.get(oid)!.push(link.target);
    }
    out.get(oid)!.sort(byRank);
  }

  const dag = breakCyclesByDfs(members, out);

  const into = new Map<Oid, Oid[]>(members.map((oid) => [oid, []]));
  for (const [u, vs] of dag) for (const v of vs) into.get(v)!.push(u);
  const layerOf = new Map<Oid, number>();
  const longest = (u: Oid): number => {
    const known = layerOf.get(u);
    if (known !== undefined) return known;
    const layer = dag.get(u)!.size ? 1 + Math.max(...[...dag.get(u)!].map(longest)) : 0;
    layerOf.set(u, layer);
    return layer;
  };
  members.forEach(longest);
  pullSourcesBesideReferrers(members, dag, into, layerOf);

  const nodes: Node[] = members.map((oid) => ({ layer: layerOf.get(oid)!, height: height(model.tables.get(oid)!), oid }));
  const index = new Map(members.map((oid, i) => [oid, i]));
  const edges: [number, number][] = [];
  const chains = new Map<string, number[]>();
  const dummies = [...dag].reduce((sum, [u, vs]) => sum + [...vs].reduce((s, v) => s + layerOf.get(u)! - layerOf.get(v)! - 1, 0), 0);
  for (const u of members) {
    for (const v of [...dag.get(u)!].sort(byRank)) {
      const span = layerOf.get(u)! - layerOf.get(v)!;
      if (span > 1 && dummies > MAX_BEND_DUMMIES) continue;
      const chain: number[] = [];
      for (let layer = layerOf.get(v)! + 1; layer < layerOf.get(u)!; layer++) chain.push(nodes.push({ layer, height: m.row, oid: null }) - 1);
      const path = [index.get(v)!, ...chain, index.get(u)!];
      for (let i = 1; i < path.length; i++) edges.push([path[i - 1]!, path[i]!]);
      if (chain.length) chains.set(pairKey(u, v), chain);
    }
  }

  const layerCount = Math.max(...nodes.map((n) => n.layer)) + 1;
  const layers: number[][] = Array.from({ length: layerCount }, () => []);
  nodes.forEach((n, i) => layers[n.layer]!.push(i));
  const left: number[][] = nodes.map(() => []);
  const right: number[][] = nodes.map(() => []);
  for (const [a, b] of edges) {
    right[a]!.push(b);
    left[b]!.push(a);
  }

  const pos = nodes.map(() => 0);
  const indexLayer = (layer: number[]) => layer.forEach((n, i) => (pos[n] = i));
  layers.forEach(indexLayer);
  for (const fromLeft of [true, false, true, false]) {
    const order = fromLeft ? layers.keys() : [...layers.keys()].reverse();
    for (const l of order) {
      const neighbors = fromLeft ? left : right;
      const bary = new Map(layers[l]!.map((n) => [n, neighbors[n]!.length ? mean(neighbors[n]!.map((k) => pos[k]!)) : pos[n]!]));
      layers[l]!.sort((a, b) => bary.get(a)! - bary.get(b)! || pos[a]! - pos[b]!);
      indexLayer(layers[l]!);
    }
  }

  const y = nodes.map(() => 0);
  for (const layer of layers) {
    let top = 0;
    for (const n of layer) {
      y[n] = top;
      top += nodes[n]!.height + m.cardGap;
    }
  }
  alignToNeighborMedians(layers, nodes, left, right, y, m.cardGap);

  const minY = Math.min(...y);
  const x = (n: number) => nodes[n]!.layer * (m.width + m.layerGap);
  const top = (n: number) => Math.round(y[n]! - minY);
  const cards = new Map<Oid, Rect>(members.map((oid, i) => [oid, { x: x(i), y: top(i), width: m.width, height: nodes[i]!.height }]));
  const bends = new Map<string, LinkBend>();
  for (const oid of members) {
    for (const id of model.incident.get(oid)!) {
      const link = model.links.get(id)!;
      const chain = chains.get(pairKey(link.source, link.target));
      if (link.source !== oid || !chain) continue;
      const points = chain.flatMap((n) => [x(n), x(n) + m.width].map((px) => ({ x: px, y: top(n) + m.row / 2 })));
      bends.set(id, { ends: [link.source, link.target], points });
    }
  }
  return {
    width: layerCount * m.width + (layerCount - 1) * m.layerGap,
    height: Math.max(...nodes.map((n, i) => top(i) + n.height)),
    cards,
    bends,
  };
}

type Node = { layer: number; height: number; oid: Oid | null };

function breakCyclesByDfs(members: readonly Oid[], out: ReadonlyMap<Oid, readonly Oid[]>): Map<Oid, Set<Oid>> {
  const dag = new Map<Oid, Set<Oid>>(members.map((oid) => [oid, new Set()]));
  const state = new Map<Oid, "open" | "done">();
  const visit = (u: Oid) => {
    state.set(u, "open");
    for (const v of out.get(u)!) {
      if (state.get(v) === "open") dag.get(v)!.add(u);
      else {
        dag.get(u)!.add(v);
        if (!state.has(v)) visit(v);
      }
    }
    state.set(u, "done");
  };
  for (const oid of members) if (!state.has(oid)) visit(oid);
  return dag;
}

function pullSourcesBesideReferrers(
  members: readonly Oid[],
  dag: ReadonlyMap<Oid, ReadonlySet<Oid>>,
  into: ReadonlyMap<Oid, readonly Oid[]>,
  layerOf: Map<Oid, number>,
): void {
  for (const u of members) {
    const referrers = into.get(u)!;
    if (!dag.get(u)!.size && referrers.length) layerOf.set(u, Math.min(...referrers.map((r) => layerOf.get(r)!)) - 1);
  }
}

function alignToNeighborMedians(
  layers: readonly number[][],
  nodes: readonly Node[],
  left: readonly number[][],
  right: readonly number[][],
  y: number[],
  gap: number,
): void {
  for (const fromLeft of [true, false]) {
    const order = fromLeft ? [...layers.keys()].slice(1) : [...layers.keys()].reverse().slice(1);
    for (const l of order) {
      const layer = layers[l]!;
      const neighbors = fromLeft ? left : right;
      const want = layer.map((n) => (neighbors[n]!.length ? median(neighbors[n]!.map((k) => y[k]!)) : y[n]!));
      const h = layer.map((n) => nodes[n]!.height);
      const down = [...want];
      for (let i = 1; i < layer.length; i++) down[i] = Math.max(want[i]!, down[i - 1]! + h[i - 1]! + gap);
      const up = [...want];
      for (let i = layer.length - 2; i >= 0; i--) up[i] = Math.min(want[i]!, up[i + 1]! - h[i]! - gap);
      layer.forEach((n, i) => (y[n] = (down[i]! + up[i]!) / 2));
    }
  }
}

function isolatedGrid(tables: ErTable[], top: number, shelfWidth: number, height: (t: ErTable) => number, m: CardMetrics, cards: Map<Oid, Rect>): Rect {
  const inset = m.frameInset;
  const columns = Math.max(1, Math.min(tables.length, Math.floor((shelfWidth - 2 * inset + m.cardGap) / (m.width + m.cardGap))));
  let rowTop = top + m.header;
  for (let i = 0; i < tables.length; i += columns) {
    const row = tables.slice(i, i + columns);
    row.forEach((t, c) => cards.set(t.oid, { x: inset + c * (m.width + m.cardGap), y: rowTop, width: m.width, height: height(t) }));
    rowTop += Math.max(...row.map(height)) + m.cardGap;
  }
  return { x: 0, y: top, width: 2 * inset + columns * m.width + (columns - 1) * m.cardGap, height: rowTop - m.cardGap + inset - top };
}

export function withMoves(layout: MapLayout, moved: ReadonlyMap<Oid, Point>): MapLayout {
  const cards = new Map([...layout.cards].map(([oid, r]) => [oid, { ...r, ...moved.get(oid) }]));
  const bends = new Map([...layout.bends].filter(([, b]) => !b.ends.some((oid) => moved.has(oid))));
  return { ...layout, cards, bends, bounds: boundsOf(cards, bends, layout.unlinkedFrame) };
}

function boundsOf(cards: ReadonlyMap<Oid, Rect>, bends: ReadonlyMap<string, LinkBend>, frame: Rect | null): Rect {
  const rects = [...cards.values(), ...(frame ? [frame] : []), ...[...bends.values()].flatMap((b) => b.points.map((p) => ({ ...p, width: 0, height: 0 })))];
  if (!rects.length) return { x: 0, y: 0, width: 0, height: 0 };
  const x = Math.min(...rects.map((r) => r.x));
  const y = Math.min(...rects.map((r) => r.y));
  return { x, y, width: Math.max(...rects.map((r) => r.x + r.width)) - x, height: Math.max(...rects.map((r) => r.y + r.height)) - y };
}

export type LinkEnd = {
  at: Point;
  facing: "left" | "right";
  marker: "many" | "one";
};

export type LinkRoute = { id: string; path: string; source: LinkEnd; target: LinkEnd; dashed: boolean };

const PARALLEL_LINK_SPACING = 6;

export function routeLinks(model: ErModel, layout: MapLayout, columns: MapColumns): readonly LinkRoute[] {
  const m = layout.metrics;
  const rowY = (oid: Oid, column: string | undefined) => {
    const r = layout.cards.get(oid)!;
    const i = visibleColumns(model.tables.get(oid)!, columns).findIndex((c) => c.name === column);
    return i < 0 ? r.y + m.header / 2 : r.y + m.header + i * m.row + m.row / 2;
  };

  const ends = [...model.links.values()].map((link) => ({ link, sy: rowY(link.source, link.pairs[0]?.source), ty: rowY(link.target, link.pairs[0]?.target) }));
  const overlapping = new Map<string, number>();
  const fanIndex = ends.map(({ link, sy, ty }) => {
    const key = link.source < link.target ? `${link.source}:${sy}:${link.target}:${ty}` : `${link.target}:${ty}:${link.source}:${sy}`;
    const i = overlapping.get(key) ?? 0;
    overlapping.set(key, i + 1);
    return { key, i };
  });

  return ends.map(({ link, sy, ty }, k) => {
    const { key, i } = fanIndex[k]!;
    const offset = (i - (overlapping.get(key)! - 1) / 2) * PARALLEL_LINK_SPACING;
    const s = layout.cards.get(link.source)!;
    const t = layout.cards.get(link.target)!;
    const sMid = s.x + s.width / 2;
    const tMid = t.x + t.width / 2;
    const loop = link.source === link.target || Math.abs(sMid - tMid) < m.width;
    const sFacing = loop || tMid > sMid ? "right" : "left";
    const tFacing = loop || tMid < sMid ? "right" : "left";
    const from = { x: sFacing === "right" ? s.x + s.width : s.x, y: sy + offset };
    const to = { x: tFacing === "right" ? t.x + t.width : t.x, y: ty + offset };

    let path: string;
    if (loop) {
      const reach = Math.max(from.x, to.x) + m.layerGap / 2;
      path = `M ${from.x} ${from.y} C ${reach} ${from.y} ${reach} ${to.y} ${to.x} ${to.y}`;
    } else {
      const bends = [...(layout.bends.get(link.id)?.points ?? [])].sort((a, b) => (tMid > sMid ? a.x - b.x : b.x - a.x));
      const points = [from, ...bends.map((p) => ({ x: p.x, y: p.y + offset })), to];
      path = `M ${from.x} ${from.y}`;
      for (let p = 1; p < points.length; p++) {
        const a = points[p - 1]!;
        const b = points[p]!;
        const mid = (a.x + b.x) / 2;
        path += ` C ${mid} ${a.y} ${mid} ${b.y} ${b.x} ${b.y}`;
      }
    }
    return {
      id: link.id,
      path,
      source: { at: from, facing: sFacing, marker: link.oneToOne ? "one" : "many" },
      target: { at: to, facing: tFacing, marker: "one" },
      dashed: link.basis === "Inferred",
    };
  });
}

function pairKey(a: Oid, b: Oid): string {
  return a < b ? `${a}:${b}` : `${b}:${a}`;
}

function mean(values: number[]): number {
  return values.reduce((a, b) => a + b, 0) / values.length;
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2;
}
