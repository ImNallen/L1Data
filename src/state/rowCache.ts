import { commands, type ResultRef, type RowPage } from "../ipc/bindings";

export const PAGE_SIZE = 256;
const MAX_PAGES = 64;
const FULL_CHARS = 4 * 1024 * 1024;

export type Row = (string | null)[];
export type RowRange = { start: number; end: number };

type Page = { rows: Row[]; cut: Set<string> };

export type RowSource = {
  page(ref: ResultRef, start: number, len: number): Promise<RowPage | "gone">;
  cell(ref: ResultRef, row: number, col: number): Promise<string | null | "gone">;
};

const ipcRowSource: RowSource = {
  async page(ref, start, len) {
    const res = await commands.resultPage(ref, start, len);
    return res.status === "ok" ? res.data : "gone";
  },
  async cell(ref, row, col) {
    const res = await commands.resultCell(ref, row, col);
    return res.status === "ok" ? res.data : "gone";
  },
};

export function pagesFor(range: RowRange, rowCount: number): number[] {
  const end = Math.min(range.end, rowCount);
  if (end <= range.start) return [];
  const pages: number[] = [];
  for (let p = Math.floor(Math.max(0, range.start) / PAGE_SIZE); p <= Math.floor((end - 1) / PAGE_SIZE); p++) pages.push(p);
  return pages;
}

export function expectedPageLength(page: number, rowCount: number): number {
  return Math.max(0, Math.min(PAGE_SIZE, rowCount - page * PAGE_SIZE));
}

const cellKey = (row: number, col: number) => `${row}:${col}`;

export class RowCache {
  readonly ref: ResultRef;
  private readonly source: RowSource;
  private readonly pages = new Map<number, Page>();
  private readonly inflight = new Map<number, Promise<void>>();
  private readonly full = new Map<string, string | null>();
  private fullChars = 0;
  private readonly listeners = new Set<() => void>();
  private version = 0;
  gone = false;

  constructor(ref: ResultRef, source: RowSource = ipcRowSource) {
    this.ref = ref;
    this.source = source;
  }

  get(row: number): Row | undefined {
    return this.pages.get(Math.floor(row / PAGE_SIZE))?.rows[row % PAGE_SIZE];
  }

  isCut(row: number, col: number): boolean {
    return this.pages.get(Math.floor(row / PAGE_SIZE))?.cut.has(cellKey(row, col)) ?? false;
  }

  async ensure(range: RowRange, rowCount: number): Promise<void> {
    if (this.gone) return;
    const wanted = pagesFor(range, rowCount);
    const loads = wanted.filter((p) => (this.pages.get(p)?.rows.length ?? -1) < expectedPageLength(p, rowCount)).map((p) => this.load(p, rowCount));
    for (const p of wanted) this.touch(p);
    await Promise.all(loads);
    this.evict(new Set(wanted));
  }

  async loadFull(row: number, col: number): Promise<string | null | undefined> {
    const key = cellKey(row, col);
    if (this.full.has(key)) return this.keepFull(key, this.full.get(key)!);
    const value = await this.source.cell(this.ref, row, col);
    if (value === "gone") {
      this.markGone();
      return undefined;
    }
    return this.keepFull(key, value);
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getVersion = (): number => this.version;

  private load(page: number, rowCount: number): Promise<void> {
    const running = this.inflight.get(page);
    if (running) return running;
    const start = page * PAGE_SIZE;
    const promise = this.source
      .page(this.ref, start, expectedPageLength(page, rowCount))
      .then((res) => {
        if (res === "gone") return this.markGone();
        const cut = new Set(res.cut.map(([r, c]) => cellKey(r, c)));
        const prev = this.pages.get(page);
        if (!prev || prev.rows.length <= res.rows.length) this.pages.set(page, { rows: res.rows, cut });
        this.notify();
      })
      .finally(() => this.inflight.delete(page));
    this.inflight.set(page, promise);
    return promise;
  }

  private keepFull(key: string, value: string | null): string | null {
    if (this.full.has(key)) this.fullChars -= this.full.get(key)?.length ?? 0;
    this.full.delete(key);
    this.full.set(key, value);
    this.fullChars += value?.length ?? 0;
    for (const [k, v] of this.full) {
      if (this.fullChars <= FULL_CHARS || k === key) break;
      this.full.delete(k);
      this.fullChars -= v?.length ?? 0;
    }
    return value;
  }

  private touch(page: number) {
    const p = this.pages.get(page);
    if (!p) return;
    this.pages.delete(page);
    this.pages.set(page, p);
  }

  private evict(keep: Set<number>) {
    const before = this.pages.size;
    for (const page of this.pages.keys()) {
      if (this.pages.size <= MAX_PAGES) break;
      if (!keep.has(page)) this.pages.delete(page);
    }
    if (this.pages.size < before) this.notify();
  }

  private markGone() {
    this.gone = true;
    this.notify();
  }

  private notify() {
    this.version++;
    for (const l of this.listeners) l();
  }

  get pageCount(): number {
    return this.pages.size;
  }
}

export type RowView = Pick<RowCache, "ref" | "get" | "isCut" | "ensure" | "loadFull" | "subscribe" | "getVersion">;

export function windowedRows(cache: RowCache, start: number, totalRowCount: number): RowView {
  return {
    ref: cache.ref,
    subscribe: cache.subscribe,
    getVersion: cache.getVersion,
    get: (row) => cache.get(start + row),
    isCut: (row, col) => cache.isCut(start + row, col),
    ensure: (range, windowLength) => cache.ensure({ start: start + range.start, end: start + Math.min(range.end, windowLength) }, totalRowCount),
    loadFull: (row, col) => cache.loadFull(start + row, col),
  };
}

const caches = new Map<string, RowCache>();

export function rowCacheFor(ref: ResultRef): RowCache {
  const key = `${ref.tabId}:${ref.runId}:${ref.index}`;
  let cache = caches.get(key);
  if (!cache) {
    for (const [k, c] of caches) if (c.ref.tabId === ref.tabId && c.ref.runId !== ref.runId) caches.delete(k);
    cache = new RowCache(ref);
    caches.set(key, cache);
  }
  return cache;
}

export function dropRowCaches(tabId: string): void {
  for (const [k, c] of caches) if (c.ref.tabId === tabId) caches.delete(k);
}
