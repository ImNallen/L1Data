import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import type { InstallReply, RestartImpact, UpdateState, UpdateStatus } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useTableEdits } from "../../state/tableEdits";
import { useWorkspace } from "../../state/workspace";
import { dropEditor } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { chooseTrack, runUpdateAction, startUpdateSync } from "./updateActions";

afterEach(() => {
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useTableEdits.setState(useTableEdits.getInitialState(), true);
  vi.unstubAllGlobals();
});

const ws = () => useWorkspace.getState();

function confirmShown() {
  const overlay = ws().overlay;
  if (overlay.kind !== "ask" || overlay.testId !== "confirm") throw new Error(`no confirm dialog, overlay is ${overlay.kind}`);
  return overlay;
}

/** What the dialog's confirm button does. */
function confirm() {
  ws().answer(confirmShown().choices[0]!.id);
}

it("asks before the restart, sends back what it showed, and asks again when more would end", async () => {
  vi.stubGlobal("window", { crypto });
  const tab = openSqlTab({ title: "orders.sql" });
  const open: RestartImpact = { running: [], transactions: [{ tabId: tab, hasWrites: false }], chats: [] };
  const changed: RestartImpact = { running: [], transactions: [{ tabId: tab, hasWrites: true }], chats: [] };
  const replies: InstallReply[] = [{ kind: "Confirm", impact: open }, { kind: "Confirm", impact: changed }, { kind: "Restarting" }];
  const calls: { cmd: string; accepted?: RestartImpact | null }[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "workspace_save") calls.push({ cmd });
    if (cmd === "update_install") {
      calls.push({ cmd, accepted: (args as { accepted: RestartImpact | null }).accepted });
      return replies.shift();
    }
    return null;
  });

  runUpdateAction({ kind: "install", version: "0.2.0" });
  await vi.waitFor(() => confirmShown());
  expect(confirmShown()).toMatchObject({
    title: "Restart to install L1 Data 0.2.0?",
    body: "Restarting ends the open transaction in “orders.sql”. L1 Data will close and reopen.",
  });

  confirm();
  await vi.waitFor(() => expect(confirmShown().body).toContain("rolls back the uncommitted changes in “orders.sql”"));
  confirm();
  await vi.waitFor(() => expect(calls).toHaveLength(5));
  expect(calls).toEqual([
    { cmd: "update_install", accepted: null },
    { cmd: "workspace_save" },
    { cmd: "update_install", accepted: open },
    { cmd: "workspace_save" },
    { cmd: "update_install", accepted: changed },
  ]);
  expect(ws().overlay.kind).toBe("none");
});

it("names the tabs with unsubmitted table edits, and asks again when another tab stages one", async () => {
  vi.stubGlobal("window", { crypto });
  const orders = openSqlTab({ title: "public.orders" });
  const customers = openSqlTab({ title: "public.customers" });
  useTableEdits.getState().addRow(orders);
  const none: RestartImpact = { running: [], transactions: [], chats: [] };
  const replies: InstallReply[] = [{ kind: "Confirm", impact: none }, { kind: "Confirm", impact: none }, { kind: "Restarting" }];
  const accepted: (RestartImpact | null)[] = [];
  mockIPC((cmd, args) => {
    if (cmd !== "update_install") return null;
    accepted.push((args as { accepted: RestartImpact | null }).accepted);
    return replies.shift();
  });

  runUpdateAction({ kind: "install", version: "0.2.0" });
  await vi.waitFor(() => confirmShown());
  expect(confirmShown().body).toBe("Restarting discards the changes in “public.orders” that haven't been submitted. L1 Data will close and reopen.");

  useTableEdits.getState().addRow(customers);
  confirm();
  await vi.waitFor(() => expect(accepted).toHaveLength(2));
  await vi.waitFor(() => confirmShown());
  expect(confirmShown().body).toBe(
    "Restarting discards the changes in “public.orders” and “public.customers” that haven't been submitted. L1 Data will close and reopen.",
  );

  confirm();
  await vi.waitFor(() => expect(accepted).toHaveLength(3));
  expect(accepted).toEqual([null, null, none]);
  expect(ws().overlay.kind).toBe("none");
});

it("stays silent on a stale click and says why a track switch was refused", async () => {
  vi.stubGlobal("window", {});
  const calls: string[] = [];
  mockIPC((cmd) => {
    calls.push(cmd);
    if (cmd === "update_download") return Promise.reject({ kind: "Stale" });
    if (cmd === "update_set_track") return Promise.reject({ kind: "Busy" });
    return null;
  });
  runUpdateAction({ kind: "download", version: "0.2.0" });
  runUpdateAction({ kind: "check" });
  await vi.waitFor(() => expect(calls).toEqual(["update_download", "update_check"]));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(ws().toast).toBeNull();
  await chooseTrack("Nightly");
  expect(ws().toast?.text).toBe("Wait for the update to finish downloading or installing before you switch tracks.");
});

it("toasts once when a download finishes, and never for a failure", () => {
  const stop = startUpdateSync();
  const at = (rev: number, status: UpdateStatus): UpdateState => ({ rev, track: "Stable", currentVersion: "0.1.0", status, checkedAtMs: null });
  const apply = (state: UpdateState) => useLive.getState().apply({ kind: "UpdateChanged", ...state });
  apply(at(1, { kind: "Downloading", version: "0.2.0", downgrade: false, percent: 99, changes: [] }));
  apply(at(2, { kind: "Failed", failure: { kind: "BadSignature" } }));
  expect(ws().toast).toBeNull();
  apply(at(3, { kind: "Downloading", version: "0.2.0", downgrade: false, percent: 99, changes: [] }));
  apply(at(4, { kind: "Downloaded", version: "0.2.0", downgrade: false, changes: [] }));
  const toast = ws().toast;
  expect(toast?.text).toBe("L1 Data 0.2.0 is ready. Click the update button in the sidebar to restart into it.");
  apply(at(5, { kind: "Downloaded", version: "0.2.0", downgrade: false, changes: [] }));
  expect(ws().toast).toBe(toast);
  stop();
});
