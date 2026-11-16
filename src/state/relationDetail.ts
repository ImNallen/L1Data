import { create } from "zustand";
import { commands, type DatabaseRef, type RelationDetail } from "../ipc/bindings";
import { detailErrorText } from "../ipc/errors";
import type { CatalogView } from "./catalog";
import { dbKey, type DbKey } from "./dbKey";

/**
 * One relation's detail as of catalog `version`. A refetch for a newer version keeps the old
 * `detail` on screen until the new one arrives, so the groups do not flash empty after DDL.
 */
export type DetailEntry = { version: number; detail: RelationDetail | null; error: string | null };

type DetailKey = string & { readonly __brand: "DetailKey" };

export function detailKey(db: DatabaseRef, oid: number): DetailKey {
  return `${dbKey(db)}\u0000${oid}` as DetailKey;
}

type DetailStore = {
  entries: Record<DetailKey, DetailEntry>;
  /**
   * Fetches `relation_detail` unless an entry at `version` or newer is held or in flight. A failed entry refetches.
   * Resolves once the detail for the entry it found or started has arrived.
   */
  ensure(db: DatabaseRef, version: number, oid: number): Promise<void>;
  /** Drops every entry whose relation the held catalogs no longer list: a dropped table, or a pruned database. */
  prune(views: Record<DbKey, CatalogView>): void;
};

const inflight = new Map<DetailKey, Promise<void>>();

export const useRelationDetail = create<DetailStore>()((set, get) => ({
  entries: {},
  async ensure(db, version, oid) {
    const key = detailKey(db, oid);
    const held = get().entries[key];
    if (held && held.version >= version && held.error === null) return inflight.get(key);
    const put = (entry: DetailEntry) => set((s) => ({ entries: { ...s.entries, [key]: entry } }));
    put({ version, detail: held?.detail ?? null, error: null });
    const fetch = (async () => {
      let next: DetailEntry;
      try {
        const res = await commands.relationDetail(db, oid);
        next =
          res.status === "ok"
            ? { version: Math.max(version, res.data.catalogVersion), detail: res.data, error: null }
            : { version, detail: null, error: detailErrorText(res.error) };
      } catch (e) {
        next = { version, detail: null, error: e instanceof Error ? e.message : String(e) };
      }
      if (get().entries[key]?.version === version) put(next);
    })();
    inflight.set(key, fetch);
    try {
      await fetch;
    } finally {
      if (inflight.get(key) === fetch) inflight.delete(key);
    }
  },
  prune(views) {
    set((s) => {
      const kept = Object.entries(s.entries).filter(([key]) => {
        const at = key.lastIndexOf("\u0000");
        return views[key.slice(0, at) as DbKey]?.relationsById.has(Number(key.slice(at + 1))) ?? false;
      });
      return kept.length === Object.keys(s.entries).length ? {} : { entries: Object.fromEntries(kept) };
    });
  },
}));
