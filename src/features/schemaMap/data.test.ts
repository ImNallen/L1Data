// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";
import type { SchemaMap } from "../../ipc/bindings";
import { dbKey } from "../../state/dbKey";
import { SHOP_SCHEMA_MAP } from "../../testing/shopSchemaMap";
import { mapKey, useSchemaMapData } from "./data";

const db = SHOP_SCHEMA_MAP.db;
const key = mapKey(db, "public");
const entry = () => useSchemaMapData.getState().entries[key];
const ensure = (version: number) => useSchemaMapData.getState().ensure(db, "public", version);

type Reply = SchemaMap | { error: unknown };

function engine(replies: Reply[]) {
  const asked: number[] = [];
  const gates: (() => void)[] = [];
  mockIPC(async (cmd, args) => {
    if (cmd !== "schema_map") throw new Error(`Unexpected IPC command: ${cmd}`);
    asked.push((args as { catalogVersion: number }).catalogVersion);
    await new Promise<void>((resolve) => gates.push(resolve));
    const reply = replies.shift()!;
    if ("error" in reply) throw reply.error;
    return reply;
  });
  return { asked, release: () => gates.shift()?.() };
}

afterEach(() => {
  clearMocks();
  useSchemaMapData.setState(useSchemaMapData.getInitialState(), true);
});

describe("the schema map store", () => {
  it("fetches once per version and keeps the old model on screen while a newer one loads", async () => {
    const { asked, release } = engine([SHOP_SCHEMA_MAP, { ...SHOP_SCHEMA_MAP, catalogVersion: 2 }]);
    const first = ensure(1);
    void ensure(1);
    expect(entry()?.status).toBe("loading");
    release();
    await first;
    expect([asked, entry()?.status, entry()?.lastModel?.version, entry()?.lastModel?.tables.size]).toEqual([[1], "ready", 1, 15]);

    const second = ensure(2);
    await Promise.resolve();
    expect([entry()?.status, entry()?.lastModel?.version]).toEqual(["loading", 1]);
    release();
    await second;
    expect([asked, entry()?.status, entry()?.lastModel?.version]).toEqual([[1, 2], "ready", 2]);
  });

  it("drops a Stale reply and keeps waiting for the next version", async () => {
    const { release } = engine([{ error: { kind: "Stale" } }]);
    const fetch = ensure(1);
    release();
    await fetch;
    expect([entry()?.status, entry()?.lastModel, entry()?.error]).toEqual(["loading", null, null]);
  });

  it("names the error, keeps the last model, and refetches the failed version", async () => {
    const { asked, release } = engine([SHOP_SCHEMA_MAP, { error: { kind: "Gone" } }, { ...SHOP_SCHEMA_MAP, catalogVersion: 2 }]);
    let fetch = ensure(1);
    release();
    await fetch;
    fetch = ensure(2);
    release();
    await fetch;
    expect([entry()?.status, entry()?.error, entry()?.lastModel?.version]).toEqual(["error", "The schema public no longer exists.", 1]);
    fetch = ensure(2);
    release();
    await fetch;
    expect([asked, entry()?.status, entry()?.lastModel?.version]).toEqual([[1, 2, 2], "ready", 2]);
  });

  it("forgets the maps of databases no catalog serves", async () => {
    const { release } = engine([SHOP_SCHEMA_MAP]);
    const fetch = ensure(1);
    release();
    await fetch;
    useSchemaMapData.getState().prune(new Set([dbKey(db)]));
    expect(entry()).toBeDefined();
    useSchemaMapData.getState().prune(new Set());
    expect(entry()).toBeUndefined();
  });
});
