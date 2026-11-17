import { create } from "zustand";
import { commands, type DatabaseRef, type MapError } from "../../ipc/bindings";
import { catalogErrorText } from "../../ipc/errors";
import { dbKey, type DbKey } from "../../state/dbKey";
import { toModel, type ErModel } from "./model";

export type MapKey = string & { readonly __brand: "MapKey" };

export function mapKey(db: DatabaseRef, schema: string | null): MapKey {
  return `${dbKey(db)}\u0000${schema ?? ""}` as MapKey;
}

function dbKeyOf(key: MapKey): DbKey {
  return key.slice(0, key.lastIndexOf("\u0000")) as DbKey;
}

export type MapData = {
  version: number;
  lastModel: ErModel | null;
  fetchedAtMs: number | null;
  status: "loading" | "ready" | "error";
  error: string | null;
};

type MapDataStore = {
  entries: Record<MapKey, MapData>;
  ensure(db: DatabaseRef, schema: string | null, version: number): Promise<void>;
  prune(keep: ReadonlySet<DbKey>): void;
};

function errorText(e: MapError, schema: string | null): string {
  switch (e.kind) {
    case "Catalog":
      return catalogErrorText(e.error);
    case "Gone":
      return schema ? `The schema ${schema} no longer exists.` : "This database has no default schema.";
    case "Stale":
      return "The catalog changed while the map loaded.";
    case "Failed":
      return e.message;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

const isStale = (error: MapError | string) => typeof error !== "string" && error.kind === "Stale";

const inflight = new Map<MapKey, Promise<void>>();

export const useSchemaMapData = create<MapDataStore>()((set, get) => ({
  entries: {},
  async ensure(db, schema, version) {
    const key = mapKey(db, schema);
    const held = get().entries[key];
    if (held && held.version >= version && held.status !== "error") return inflight.get(key);
    const put = (entry: MapData) => set((s) => ({ entries: { ...s.entries, [key]: entry } }));
    put({ version, lastModel: held?.lastModel ?? null, fetchedAtMs: held?.fetchedAtMs ?? null, status: "loading", error: null });
    const fetch = (async () => {
      let reply: { model: ErModel } | { error: MapError | string };
      try {
        const res = await commands.schemaMap(db, schema, version);
        reply = res.status === "ok" ? { model: toModel(res.data) } : { error: res.error };
      } catch (e) {
        reply = { error: e instanceof Error ? e.message : String(e) };
      }
      const current = get().entries[key];
      if (current?.version !== version || current.status !== "loading") return;
      if ("error" in reply && isStale(reply.error)) return;
      if ("model" in reply) put({ version, lastModel: reply.model, fetchedAtMs: Date.now(), status: "ready", error: null });
      else put({ ...current, status: "error", error: typeof reply.error === "string" ? reply.error : errorText(reply.error, schema) });
    })();
    inflight.set(key, fetch);
    try {
      await fetch;
    } finally {
      if (inflight.get(key) === fetch) inflight.delete(key);
    }
  },
  prune(keep) {
    set((s) => {
      const kept = Object.entries(s.entries).filter(([key]) => keep.has(dbKeyOf(key as MapKey)));
      return kept.length === Object.keys(s.entries).length ? {} : { entries: Object.fromEntries(kept) };
    });
  },
}));
