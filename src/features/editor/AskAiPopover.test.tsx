// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { commands, type AskOutcome, type ChatOptions, type ReplyBlock } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace, type Tab } from "../../state/workspace";
import { cellsState } from "../../testing/notebooks";
import { CommandPalette } from "../../shell/CommandPalette";
import { openCommandPalette } from "../../shell/commands";
import { preloadSurfaces } from "../../shell/lazySurfaces";
import { AskAi } from "./AskAiPopover";
import { applyAsk, discardAsk, openAskAi, setAskPrompt, startAskAiSync, submitAsk, useAskAi } from "./askAi";
import { activateEditor, dropEditor, seedEditor, textOf } from "./editorStates";

const TAB: Tab = { id: "ask-overlay-cell", title: "Cell", console: { db: null, schema: null, mode: "AutoCommit" }, table: null };
let stop = () => {};

beforeAll(preloadSurfaces);

beforeEach(() => {
  mockIPC(() => null);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  Element.prototype.scrollIntoView = () => {};
  useWorkspace.setState(cellsState([TAB]));
  seedEditor(TAB.id, "SELECT 1;");
  activateEditor(TAB.id);
  stop = startAskAiSync();
});

afterEach(() => {
  cleanup();
  stop();
  discardAsk();
  dropEditor(TAB.id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  clearMocks();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

async function propose(ask: AskOutcome = { kind: "Replacement", sql: "SELECT 2;" }, blocks: ReplyBlock[] = []) {
  const options: ChatOptions = { provider: "Claude", model: "opus", effort: "high", permission: "AskFirst" };
  vi.spyOn(commands, "assistantOpen").mockImplementation(async (id) => ({
    status: "ok",
    data: { state: { id, rev: 1, title: "Ask", updatedAtMs: 1, entryCount: 0, status: { kind: "Draft" }, options }, entries: [] },
  }));
  vi.spyOn(commands, "assistantAsk").mockResolvedValue({ status: "ok", data: { turn: 5 } });
  openAskAi();
  setAskPrompt("Edit");
  await submitAsk();
  const state = useAskAi.getState().state;
  if (state.kind === "closed" || !state.session.chatId) throw Error("No session");
  const chatId = state.session.chatId;
  if (blocks.length) useLive.getState().apply({ kind: "ChatEntryChanged", chatId, seq: 6, turn: 5, rev: 2, entry: { kind: "Text", done: true, blocks } });
  useLive.getState().apply({
    kind: "ChatEntryChanged",
    chatId,
    seq: 7,
    turn: 5,
    rev: 3,
    entry: { kind: "End", atMs: 1, outcome: { kind: "Completed" }, suggestions: [], ask },
  });
  return chatId;
}

it("keeps the Ask conversation in the popover across questions and proposals", async () => {
  render(<AskAi />);
  const layout = useWorkspace.getState().layout;
  const chatId = await act(() =>
    propose({ kind: "Answer" }, [
      { kind: "Markdown", text: "It returns **one** row." },
      { kind: "Sql", sql: "SELECT 2;", relations: [], vetted: true, complete: true },
    ]),
  );
  useLive.getState().apply({
    kind: "ChatEntryChanged",
    chatId,
    seq: 5,
    turn: 5,
    rev: 4,
    entry: { kind: "User", atMs: 1, text: "What does this return?", context: null, attachment: null },
  });
  const thread = await screen.findByTestId("ask-ai-thread");
  expect(thread.textContent).toContain("What does this return?");
  expect(thread.textContent).toContain("It returns one row.");
  expect(thread.textContent).toContain("SELECT 2;");
  expect(screen.queryByTestId("sql-run-here")).toBeNull();
  expect(useWorkspace.getState().layout).toBe(layout);
  expect(document.activeElement).toBe(screen.getByLabelText("Ask a question or describe a change"));
});

it.each(["prompt", "review"])("lets CommandPalette handle Escape while holding the Ask %s", async (kind) => {
  render(
    <>
      <AskAi />
      <CommandPalette />
    </>,
  );
  await act(async () => {
    if (kind === "review") await propose();
    else openAskAi();
  });
  act(openCommandPalette);
  expect(screen.getByTestId("command-palette")).toBeTruthy();
  expect(screen.queryByTestId("ask-ai-review")).toBeNull();
  expect(screen.queryByTestId("ask-ai-popover")).toBeNull();
  expect(applyAsk()).toBe(false);
  fireEvent.keyDown(screen.getByTestId("command-input"), { key: "Escape" });
  expect(screen.queryByTestId("command-palette")).toBeNull();
  expect(useAskAi.getState().state.kind).toBe(kind);
  expect(screen.getByTestId(kind === "review" ? "ask-ai-review" : "ask-ai-popover")).toBeTruthy();
});

it("suspends Ask in settings and restores its held review without applying SQL", async () => {
  render(<AskAi />);
  await act(propose);
  act(() => useWorkspace.getState().openSettings());
  expect(screen.queryByTestId("ask-ai-review")).toBeNull();
  expect(applyAsk()).toBe(false);
  fireEvent.keyDown(window, { key: "Escape" });
  expect(useAskAi.getState().state.kind).toBe("review");
  expect(textOf(TAB.id)).toBe("SELECT 1;");
  act(() => useWorkspace.getState().closeSettings());
  expect(screen.getByTestId("ask-ai-review")).toBeTruthy();
  act(() => {
    expect(applyAsk()).toBe(true);
  });
  expect(textOf(TAB.id)).toBe("SELECT 2;");
});
