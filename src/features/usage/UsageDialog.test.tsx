// @vitest-environment jsdom
import { openWorkspaceItem } from "../../testing/workspaceUI";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { LimitsReport } from "../../ipc/bindings";
import { commandForKey } from "../../shell/commands";
import { Overlays } from "../../shell/Overlays";
import { useWorkspace } from "../../state/workspace";
import { SavedPalette } from "../saved/SavedPalette";
import { Sidebar } from "../sidebar/Sidebar";
import { useLimits } from "./useLimits";

const report: LimitsReport = {
  checkedAtMs: Date.now(),
  sources: [{ source: "Claude", name: "Claude", outcome: { kind: "NoLimits", plan: null } }],
};
const read = vi.fn<() => Promise<LimitsReport>>();

beforeEach(() => {
  read.mockReset().mockResolvedValue(report);
  mockIPC((command) => {
    if (command === "usage_limits") return read();
  });
});

afterEach(() => {
  cleanup();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLimits.setState(useLimits.getInitialState(), true);
  clearMocks();
  vi.useRealTimers();
});

function Shell() {
  return (
    <>
      <Sidebar hosted={null} />
      <Overlays />
      <SavedPalette />
    </>
  );
}

it("opens Usage from the sidebar without entering Settings, closes with Escape and Close, and restores focus", async () => {
  const user = userEvent.setup();
  render(<Shell />);
  expect(read).not.toHaveBeenCalled();
  const opener = screen.getByRole("button", { name: "Workspace options" });
  await openWorkspaceItem(user, "open-usage");
  expect(screen.getByRole("dialog", { name: "Usage" })).toBe(screen.getByTestId("usage-dialog"));
  expect(useWorkspace.getState().screen).toEqual({ kind: "workspace" });
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));
  await screen.findByTestId("usage-note-Claude");
  expect(read).toHaveBeenCalledTimes(1);
  act(() => useWorkspace.getState().open({ kind: "usage" }));
  expect(read).toHaveBeenCalledTimes(1);
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("usage-dialog")).toBeNull();
  await waitFor(() => expect(document.activeElement).toBe(opener));
  await openWorkspaceItem(user, "open-usage");
  await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  await user.click(screen.getByRole("button", { name: "Close" }));
  await waitFor(() => expect(document.activeElement).toBe(opener));
  expect(screen.queryByTestId("usage-dialog")).toBeNull();
});

it("keeps cached cards while refreshing, disables duplicate reads, and displays a failed refresh", async () => {
  const user = userEvent.setup();
  render(<Shell />);
  await openWorkspaceItem(user, "open-usage");
  await screen.findByTestId("usage-note-Claude");
  let rejectRead: ((error: Error) => void) | undefined;
  read.mockImplementationOnce(
    () =>
      new Promise((_resolve, reject) => {
        rejectRead = reject;
      }),
  );
  await user.click(screen.getByTestId("usage-refresh"));
  expect(read).toHaveBeenCalledTimes(2);
  expect(screen.getByTestId("usage-refresh")).toHaveProperty("disabled", true);
  expect(screen.getByTestId("usage-checked").textContent).toBe("Checking…");
  expect(screen.getByTestId("usage-note-Claude")).toBeTruthy();
  await user.click(screen.getByTestId("usage-refresh"));
  expect(read).toHaveBeenCalledTimes(2);
  await act(async () => rejectRead?.(new Error("IPC down")));
  expect(screen.getByTestId("usage-error").textContent).toContain("IPC down");
  expect(screen.getByTestId("usage-note-Claude")).toBeTruthy();
  await user.click(screen.getByTestId("usage-refresh"));
  await waitFor(() => expect(read).toHaveBeenCalledTimes(3));
  expect(screen.queryByTestId("usage-error")).toBeNull();
});

it("shows loading on first open and retains an in-flight read and cached report across dismissal", async () => {
  let finish: ((value: LimitsReport) => void) | undefined;
  read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const user = userEvent.setup();
  render(<Shell />);
  await openWorkspaceItem(user, "open-usage");
  expect(screen.getByTestId("usage-loading")).toBeTruthy();
  await user.keyboard("{Escape}");
  await openWorkspaceItem(user, "open-usage");
  expect(read).toHaveBeenCalledTimes(1);
  await act(async () => finish?.(report));
  await screen.findByTestId("usage-note-Claude");
  await user.keyboard("{Escape}");
  read.mockImplementationOnce(() => new Promise(() => {}));
  await openWorkspaceItem(user, "open-usage");
  expect(screen.getByTestId("usage-note-Claude")).toBeTruthy();
  expect(screen.queryByTestId("usage-loading")).toBeNull();
  expect(read).toHaveBeenCalledTimes(2);
});

it("replaces Usage with the saved query popup when its shortcut runs", async () => {
  const user = userEvent.setup();
  render(<Shell />);
  await openWorkspaceItem(user, "open-usage");
  await screen.findByTestId("usage-note-Claude");
  act(() => commandForKey({ key: "e", ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }, false)?.run());
  expect(screen.queryByTestId("usage-dialog")).toBeNull();
  expect(screen.getByTestId("saved-input")).toBeTruthy();
  await act(async () => new Promise((resolve) => setTimeout(resolve, 0)));
  expect(document.activeElement).toBe(screen.getByTestId("saved-input"));
});

it("refreshes after a window resets while open and stops the reset timer on dismissal", async () => {
  vi.useFakeTimers();
  const resetting = (now: number): LimitsReport => ({
    checkedAtMs: now,
    sources: [
      {
        source: "Claude",
        name: "Claude",
        outcome: {
          kind: "Ready",
          plan: null,
          windows: [{ id: "session", label: "Session", leftPercent: 10, level: "Low", resetsAtMs: now + 1000 }],
        },
      },
    ],
  });
  read.mockResolvedValueOnce(resetting(Date.now())).mockResolvedValue(report);
  render(<Shell />);
  await act(async () => useWorkspace.getState().open({ kind: "usage" }));
  expect(read).toHaveBeenCalledTimes(1);
  await act(async () => vi.advanceTimersByTimeAsync(1999));
  expect(read).toHaveBeenCalledTimes(1);
  await act(async () => vi.advanceTimersByTimeAsync(1));
  expect(read).toHaveBeenCalledTimes(2);
  act(() => useWorkspace.getState().close());
  read.mockResolvedValueOnce(resetting(Date.now()));
  await act(async () => useWorkspace.getState().open({ kind: "usage" }));
  expect(read).toHaveBeenCalledTimes(3);
  act(() => useWorkspace.getState().close());
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(read).toHaveBeenCalledTimes(3);
});
