import { expect, it } from "vitest";
import type { ApproveError, ChatError, FileWriteError, SaveTarget, SendError, UpdateFailure } from "./bindings";
import {
  applyErrorText,
  readOnlyText,
  approveErrorText,
  chatErrorText,
  configureErrorText,
  connectErrorText,
  fileWriteErrorText,
  runErrorText,
  sendErrorText,
  updateErrorText,
  updateFailureText,
} from "./errors";

it("keeps the engine's actionable capability refusal intact", () => {
  const what = "SQL Server Manual mode is unavailable. Select Auto-commit and use explicit transaction statements.";
  expect(configureErrorText({ kind: "Unsupported", what })).toBe(what);
  expect(applyErrorText({ kind: "Unsupported", what })).toBe(what);
  expect(runErrorText({ kind: "Unsupported", what })).toBe(`Cannot run this SQL. ${what}`);
});

it("tells the user to verify unknown edit outcomes before retrying", () => {
  const error = { message: "Connection closed during commit", severity: "ERROR", sqlstate: null, detail: null, hint: null, context: null, position: null };
  expect(applyErrorText({ kind: "OutcomeUnknown", error })).toBe(
    "The outcome of these edits is unknown. Verify the saved data before retrying. Connection closed during commit",
  );
  expect(applyErrorText({ kind: "Transaction", error })).not.toContain("Nothing was saved");
});

it("says what to do about every way a send can fail", () => {
  const errors: SendError[] = [
    { kind: "Busy" },
    { kind: "EmptyMessage" },
    { kind: "ProviderOff", provider: "Claude" },
    { kind: "UnknownModel", model: "opus-4" },
    { kind: "AttachmentGone" },
    { kind: "Store", message: "database is locked" },
  ];
  expect(errors.map(sendErrorText)).toEqual([
    "The assistant is still answering. Stop it or wait for it to finish.",
    "Type a message first.",
    "Claude is turned off. Turn it on in Settings → Providers.",
    "The model opus-4 is no longer offered. Pick another model.",
    "The attached result is gone. Run the query again, then attach it.",
    "The chat could not be saved. database is locked",
  ]);
});

it("says why a chat could not be opened or configured, and why an approval missed", () => {
  const errors: ChatError[] = [
    { kind: "UnknownModel", model: "opus-4" },
    { kind: "UnsupportedEffort", effort: "max" },
    { kind: "Store", message: "disk full" },
  ];
  expect(errors.map(chatErrorText)).toEqual([
    "The model opus-4 is no longer offered. Pick another model.",
    "This model has no max effort level.",
    "The chat could not be saved. disk full",
  ]);
  const late: ApproveError = { kind: "NotPending" };
  expect(approveErrorText(late)).toBe("This query was already answered, stopped, or expired.");
});

it("names the host key an SSH server offered, and the fix for a changed one", () => {
  const server = { host: "bastion", port: 2222, algorithm: "ssh-ed25519", fingerprint: "SHA256:abc" };
  expect(connectErrorText({ kind: "Ssh", error: { kind: "HostKeyUnknown", ...server, trust: 1 } })).toBe(
    "SSH: bastion:2222 offered an unknown ssh-ed25519 host key SHA256:abc.",
  );
  expect(connectErrorText({ kind: "Ssh", error: { kind: "HostKeyChanged", ...server, knownAt: "/Users/a/.ssh/known_hosts:4" } })).toContain(
    "ssh-keygen -R '[bastion]:2222' -f '/Users/a/.ssh/known_hosts'",
  );
  expect(connectErrorText({ kind: "Ssh", error: { kind: "MissingSecret" } })).toMatch(/^SSH: /);
});

it("says what went wrong with an update, and nothing for a click the update already moved past", () => {
  const failures: UpdateFailure[] = [
    { kind: "Offline" },
    { kind: "BadSignature" },
    { kind: "ReadOnlyLocation" },
    { kind: "PermissionDenied" },
    { kind: "Other", message: "binary for the current target not found in the archive" },
  ];
  expect(failures.map(updateFailureText)).toEqual([
    "Couldn't reach GitHub for updates.",
    "The update's signature didn't match, so it was discarded.",
    "L1 Data can't replace itself where it runs. On macOS, move it to Applications.",
    "L1 Data couldn't replace itself. Check that you can change the folder it runs from.",
    "The update failed: binary for the current target not found in the archive",
  ]);
  expect(updateErrorText({ kind: "Stale" })).toBeNull();
  expect(updateErrorText({ kind: "Busy" })).toBe("Wait for the update to finish downloading or installing before you switch tracks.");
});

it("points a failed save at Save As, and a failed Save As elsewhere", () => {
  const binding = { path: "/q/a.sql", stamp: "s1", format: { bom: false, crlf: false } };
  const save: SaveTarget = { kind: "Bound", binding, overwrite: null };
  const saveAs: SaveTarget = { kind: "New", path: "/q/a.sql", format: null, taken: [] };
  const errors: FileWriteError[] = [{ kind: "NotFound" }, { kind: "PermissionDenied" }];
  expect(errors.map((e) => fileWriteErrorText(e, save))).toEqual([
    "The folder for /q/a.sql no longer exists. Use Save As to pick another place.",
    "You don't have permission to save /q/a.sql. Use Save As to save a copy elsewhere.",
  ]);
  expect(errors.map((e) => fileWriteErrorText(e, saveAs))).toEqual(["The folder for /q/a.sql doesn't exist.", "You don't have permission to save /q/a.sql."]);
});

it("names the table, object, and foreign key that refuse a SQL Server edit", () => {
  const trigger = { kind: "Trigger" as const, table: "dbo.order_items", trigger: "dbo.items_audit" };
  expect(applyErrorText({ kind: "Refused", reason: { kind: "Cascade", foreignKey: "FK_items_order", table: "dbo.order_items", cause: trigger } })).toBe(
    "Nothing was applied. Edits are unavailable here: FK_items_order changes rows in dbo.order_items, and dbo.order_items has an enabled trigger, dbo.items_audit, which could commit or roll back them.",
  );
  expect(readOnlyText({ kind: "Identity" })).toBe("Read-only: identity");
  expect(applyErrorText({ kind: "RowChanged", change: { kind: "Update", index: 0 } })).toContain("changed since it was read");
  expect(applyErrorText({ kind: "Session", reason: "OpenTransaction" })).toContain("Commit or roll back");
  expect(applyErrorText({ kind: "Cancelled" })).toBe("Cancelled. Nothing was applied.");
});
