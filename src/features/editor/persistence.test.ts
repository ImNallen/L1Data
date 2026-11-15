import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useWorkspace } from "../../state/workspace";
import { flushBeforeClose, flushWorkspaceSave } from "./persistence";

let saves: number;

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  vi.useFakeTimers();
  saves = 0;
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

const settledAfter = async (ms: number, p: Promise<void>) => {
  let done = false;
  void p.then(() => (done = true));
  await vi.advanceTimersByTimeAsync(ms);
  return done;
};

it("writes the workspace and lets a file save under way finish before the window goes", async () => {
  mockIPC((cmd) => {
    if (cmd === "workspace_save") saves++;
  });
  let finishSave = () => {};
  const inFlight = new Promise<void>((r) => (finishSave = r));
  const closing = flushBeforeClose(() => inFlight);
  expect(await settledAfter(500, closing)).toBe(false);
  finishSave();
  expect([await settledAfter(0, closing), saves]).toEqual([true, 1]);
});

it("lets the window go after two seconds when the engine never answers", async () => {
  mockIPC(() => new Promise(() => {}));
  const closing = flushBeforeClose(() => new Promise(() => {}));
  expect(await settledAfter(1_999, closing)).toBe(false);
  expect(await settledAfter(1, closing)).toBe(true);
});

it("reports a refused workspace save once until a save succeeds", async () => {
  let refuse = true;
  mockIPC((cmd) => {
    if (cmd === "workspace_save" && refuse) return Promise.reject({ kind: "Store", message: "sqlite: database or disk is full" });
  });
  const toast = () => useWorkspace.getState().toast;
  const text = "Couldn't save the workspace: sqlite: database or disk is full. Changes since the last save won't reopen after you quit.";

  await flushWorkspaceSave();
  const first = toast();
  await flushWorkspaceSave();
  expect([first?.text, toast()]).toEqual([text, first]);

  refuse = false;
  await flushWorkspaceSave();
  refuse = true;
  await flushWorkspaceSave();
  expect(toast()).toEqual({ id: first!.id + 1, text });
});

it("lets the window go when the workspace save fails", async () => {
  mockIPC(() => {
    throw new Error("bridge down");
  });
  await expect(flushBeforeClose(() => Promise.reject(new Error("save failed")))).resolves.toBeUndefined();
});
