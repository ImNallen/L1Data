import { afterEach, expect, it } from "vitest";
import { ask, confirm, useWorkspace, type Question } from "./workspace";

const ws = () => useWorkspace.getState();
const flush = () => new Promise((r) => setTimeout(r, 0));
const q = (title: string): Question<"yes" | "no"> => ({
  title,
  body: "",
  choices: [
    { id: "no", label: "No", variant: "ghost" },
    { id: "yes", label: "Yes", variant: "primary" },
  ],
});
const shown = () => {
  const o = ws().overlay;
  return o.kind === "ask" ? o.title : o.kind;
};

afterEach(() => useWorkspace.setState(useWorkspace.getInitialState(), true));

it("resolves with the chosen id and closes the question", async () => {
  const answer = ask(q("Save?"));
  expect(shown()).toBe("Save?");
  ws().answer("yes");
  expect(shown()).toBe("none");
  expect(await answer).toBe("yes");
});

it("answers null when the question is cancelled, replaced, or swept away by another screen", async () => {
  const cancelled = ask(q("a"));
  ws().close();
  const replaced = ask(q("b"));
  ws().open({ kind: "queries", tab: "history" });
  const settings = ask(q("c"));
  ws().openSettings();
  const search = ask(q("d"));
  ws().open({ kind: "navigation" });
  const next = ask(q("e"));
  const superseded = ask(q("f"));
  expect(await Promise.all([cancelled, replaced, settings, search, next])).toEqual([null, null, null, null, null]);
  expect(shown()).toBe("f");
  ws().answer("no");
  expect(await superseded).toBe("no");
});

it("settles once, so a second answer or a later close changes nothing", async () => {
  const settled: (string | null)[] = [];
  void ask(q("once")).then((c) => settled.push(c));
  ws().answer("yes");
  ws().answer("no");
  ws().close();
  await flush();
  expect(settled).toEqual(["yes"]);
});

it("lets a continuation ask the next question without the first one's close sweeping it away", async () => {
  const second = ask(q("first")).then(() => ask(q("second")));
  ws().answer("yes");
  await flush();
  expect(shown()).toBe("second");
  ws().answer("no");
  expect(await second).toBe("no");
});

it("keeps confirm's test ids and resolves true only for its button", async () => {
  const yes = confirm({ title: "Delete?", body: "Gone for good.", confirmLabel: "Delete" });
  expect(ws().overlay).toMatchObject({ kind: "ask", testId: "confirm", choices: [{ id: "btn", label: "Delete", variant: "danger" }] });
  ws().answer("btn");
  const no = confirm({ title: "Delete?", body: "", confirmLabel: "Delete" });
  ws().close();
  expect([await yes, await no]).toEqual([true, false]);
});
