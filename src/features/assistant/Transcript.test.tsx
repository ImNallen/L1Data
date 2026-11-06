// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, expect, it } from "vitest";
import type { ChatState, ChatStatus, Entry, EntrySnapshot } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { Transcript } from "./Transcript";

const ID = "0b7d5a52-9f1e-4c55-9d43-3c1f3c6e2a10";

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
});

afterEach(() => {
  cleanup();
  useLive.setState(useLive.getInitialState(), true);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

const snap = (seq: number, turn: number, entry: Entry): EntrySnapshot => ({ chatId: ID, seq, rev: seq, turn, entry });
const user = (seq: number, turn: number, text: string) => snap(seq, turn, { kind: "User", atMs: 1, text, context: null, attachment: null });
const chat = (status: ChatStatus): ChatState => ({
  id: ID,
  rev: 1,
  title: "t",
  updatedAtMs: 1,
  options: { provider: "Claude", model: "opus", effort: "high", permission: "AskFirst" },
  status,
  entryCount: 0,
});

function show(status: ChatStatus, entries: EntrySnapshot[]) {
  const state = chat(status);
  act(() => useLive.setState({ chats: { [ID]: state }, transcripts: { [ID]: Object.fromEntries(entries.map((e) => [e.seq, e])) } }));
  return state;
}

const said = () => [screen.getByTestId("assistant-announcement").textContent, screen.getByTestId("assistant-alert").textContent];

const FINISHED = [
  user(0, 0, "Count orders"),
  snap(1, 0, { kind: "Text", done: true, blocks: [{ kind: "Markdown", text: "There are **12** orders." }] }),
  snap(2, 0, { kind: "End", atMs: 2, outcome: { kind: "Completed" }, suggestions: [] }),
];

it("keeps the streamed transcript quiet and says each turn's moments once, never the history it opens with", () => {
  const { rerender } = render(<Transcript chat={show({ kind: "Idle" }, FINISHED)} />);
  expect(screen.getByRole("log", { name: "Chat transcript" }).getAttribute("aria-live")).toBe("off");
  expect(said()).toEqual(["", ""]);

  const asked = [...FINISHED, user(3, 1, "Run it")];
  rerender(<Transcript chat={show({ kind: "Running", turn: 1, startedAtMs: 1, phase: "Starting" }, asked)} />);
  expect(said()).toEqual(["Starting Claude…", ""]);

  const waiting = [
    ...asked,
    snap(4, 1, {
      kind: "Tool",
      call: "toolu_1",
      tool: { kind: "RunQuery", sql: "select count(*) from orders", purpose: "Count the orders", relations: ["orders"] },
      status: { kind: "AwaitingApproval", approval: 1, reason: { kind: "AskFirst" } },
    }),
  ];
  rerender(<Transcript chat={show({ kind: "Running", turn: 1, startedAtMs: 1, phase: "AwaitingApproval" }, waiting)} />);
  expect(said()).toEqual(["Starting Claude…", "Claude asks to run a query: Count the orders. Run or deny it in the chat."]);
  expect(screen.getByRole("group", { name: "Run this query?" })).toBeTruthy();

  rerender(<Transcript chat={show({ kind: "Running", turn: 1, startedAtMs: 1, phase: "Working" }, waiting)} />);
  expect(said()).toEqual(["Starting Claude…", ""]);

  const replied = [
    ...waiting,
    snap(5, 1, { kind: "Text", done: true, blocks: [{ kind: "Markdown", text: "## Result\n\n| n |\n|---|\n| 12 |" }] }),
    snap(6, 1, { kind: "End", atMs: 3, outcome: { kind: "Completed" }, suggestions: [] }),
  ];
  rerender(<Transcript chat={show({ kind: "Idle" }, replied)} />);
  expect(said()).toEqual(["Claude replied. Result n 12", ""]);

  const failed = [
    ...replied,
    user(7, 2, "Again"),
    snap(8, 2, { kind: "End", atMs: 4, outcome: { kind: "Failed", error: { kind: "SignedOut" } }, suggestions: [] }),
  ];
  rerender(<Transcript chat={show({ kind: "Idle" }, failed)} />);
  expect(said()[1]).toMatch(/^Claude failed\. Claude is signed out\./);
});

it("introduces an empty conversation without claiming to send editor SQL", () => {
  render(<Transcript chat={show({ kind: "Idle" }, [])} />);
  const introduction = screen.getByRole("log", { name: "Chat transcript" }).textContent;
  expect(introduction).toBe("Ask for help with your database.");
  expect(introduction).not.toContain("SQL");
});
