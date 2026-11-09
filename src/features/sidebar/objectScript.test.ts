import { afterEach, expect, it, vi } from "vitest";
import { commands, type DatabaseRef } from "../../ipc/bindings";
import { activeId, useWorkspace } from "../../state/workspace";
import { dropEditor, textOf } from "../editor/editorStates";
import { openObjectScript } from "./ConnectionTree";

const shop: DatabaseRef = { connectionId: "pg", database: "shop" };

afterEach(() => {
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  vi.restoreAllMocks();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

it("opens the script in a new tab on the object's database, titled by the engine", async () => {
  const text = "CREATE TABLE public.orders (\n    id bigint NOT NULL\n);\n";
  const call = vi.spyOn(commands, "objectScript").mockResolvedValueOnce({ status: "ok", data: { title: "orders.sql", text, warnings: [] } });
  await openObjectScript(shop, { kind: "Relation", oid: 42 });
  expect(call).toHaveBeenCalledWith(shop, { kind: "Relation", oid: 42 });
  const active = activeId(useWorkspace.getState());
  expect(useWorkspace.getState().tabs[active!]).toMatchObject({ title: "orders.sql", console: { db: shop }, table: null });
  expect(textOf(active!)).toBe(text);
  expect(useWorkspace.getState().toast).toBeNull();
});

it("opens the script and names in a toast each property it could not script", async () => {
  const text = "-- L1 Data could not script: privileges on public.t\n\nCREATE TABLE public.t ();\n";
  vi.spyOn(commands, "objectScript").mockResolvedValueOnce({ status: "ok", data: { title: "t.sql", text, warnings: ["privileges on public.t"] } });
  await openObjectScript(shop, { kind: "Relation", oid: 9 });
  const { toast } = useWorkspace.getState();
  expect(textOf(activeId(useWorkspace.getState())!)).toBe(text);
  expect(toast?.text).toBe("L1 Data could not script: privileges on public.t.");
});

it("shows why an object cannot be scripted and opens no tab", async () => {
  vi.spyOn(commands, "objectScript").mockResolvedValueOnce({ status: "error", error: { kind: "Failed", message: "Aggregates cannot be scripted yet." } });
  await openObjectScript(shop, { kind: "Routine", oid: 7 });
  const { tabs, toast } = useWorkspace.getState();
  expect(tabs).toEqual({});
  expect(toast?.text).toBe("Aggregates cannot be scripted yet.");
});
