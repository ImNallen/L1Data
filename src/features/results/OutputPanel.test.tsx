// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it } from "vitest";
import type { OutputBody, OutputEntry } from "../../ipc/bindings";
import { OUTPUT_LIMIT, useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { OutputPanel } from "./OutputPanel";
import { ResultsPane } from "./ResultsPane";
import { cellsState } from "../../testing/notebooks";

const at = new Date(2026, 9, 15, 9, 5, 7).getTime();
const entry = (runId: number, body: OutputBody): OutputEntry => ({ atMs: at, runId, body });

const lines = (entries: OutputEntry[]) =>
  renderToStaticMarkup(<OutputPanel log={{ entries, dropped: 0 }} />)
    .split('<div data-testid="output-line"')
    .slice(1)
    .map((html) => {
      const attrs = html.slice(0, html.indexOf(">"));
      return {
        kind: attrs.match(/data-kind="(\w+)"/)?.[1],
        severity: attrs.match(/data-severity="(\w+)"/)?.[1],
        runStart: attrs.includes("border-t"),
        text: html
          .slice(attrs.length + 1)
          .replace(/<[^>]+>/g, "")
          .replace(/&gt;/g, ">"),
      };
    });

it("prints each entry as a timestamped line in DataGrip's Output order", () => {
  const error = { severity: "ERROR", sqlstate: "22012", message: "division by zero", detail: null, hint: null, context: null, position: null };
  expect(
    lines([
      entry(1, { kind: "Statement", statement: 0, text: "select slow_echo(0.01)" }),
      entry(1, { kind: "Notice", severity: "WARNING", message: "careful", detail: "more" }),
      entry(1, { kind: "Rows", statement: 0, rows: 200_000, elapsedMs: 1_200, truncated: true }),
      entry(1, { kind: "Info", message: "BEGIN (implicit)" }),
      entry(1, { kind: "Command", statement: 1, tag: "UPDATE 3", elapsedMs: 4 }),
      entry(2, { kind: "Statement", statement: 0, text: "select 1/0" }),
      entry(2, { kind: "Error", statement: 0, error }),
    ]),
  ).toEqual([
    { kind: "Statement", severity: undefined, runStart: false, text: "09:05:07>select slow_echo(0.01)" },
    { kind: "Notice", severity: "WARNING", runStart: false, text: "09:05:07WARNING:carefulmore" },
    { kind: "Rows", severity: undefined, runStart: false, text: "09:05:07200,000 rows, past the result limit · 1.20 s" },
    { kind: "Info", severity: undefined, runStart: false, text: "09:05:07BEGIN (implicit)" },
    { kind: "Command", severity: undefined, runStart: false, text: "09:05:07UPDATE 3 · 4 ms" },
    { kind: "Statement", severity: undefined, runStart: true, text: "09:05:07>select 1/0" },
    { kind: "Error", severity: "ERROR", runStart: false, text: "09:05:07ERROR:division by zeroSQLSTATE 22012" },
  ]);
});

it("says lines were dropped only once the log has dropped some", () => {
  const full = Array.from({ length: OUTPUT_LIMIT }, () => entry(1, { kind: "Info", message: "x" }));
  expect(renderToStaticMarkup(<OutputPanel log={{ entries: full, dropped: 0 }} />)).not.toContain("Only the last");
  expect(renderToStaticMarkup(<OutputPanel log={{ entries: full, dropped: 1 }} />)).toContain("Only the last 5,000 lines are kept.");
});

afterEach(() => {
  cleanup();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

it("leaves the lines on screen untouched while batches arrive past the cap", () => {
  const info = (n: number) => entry(1, { kind: "Info", message: `line ${n}` });
  const send = (from: number, count: number) =>
    act(() => useLive.getState().apply({ kind: "ConsoleOutput", tabId: "t1", entries: Array.from({ length: count }, (_, i) => info(from + i)) }));
  const tab = { id: "t1", title: "Query", console: { db: { connectionId: "c1", database: "shop" }, mode: "AutoCommit" as const }, table: null };
  useWorkspace.setState(cellsState([tab]));
  send(0, OUTPUT_LIMIT);
  render(<ResultsPane tabId="t1" />);
  const panel = screen.getByTestId("output-panel");
  const kept = [...panel.querySelectorAll("[data-testid=output-line]")].find((line) => line.textContent?.endsWith("line 4000"))!;
  const mutations = new MutationObserver(() => {});
  mutations.observe(panel, { subtree: true, childList: true, characterData: true });

  send(OUTPUT_LIMIT, 10);

  const changed = mutations.takeRecords();
  expect(kept.isConnected && kept.textContent?.endsWith("line 4000")).toBe(true);
  expect(changed.filter((m) => m.target !== panel)).toEqual([]);
  expect(changed.flatMap((m) => [...m.addedNodes]).map((n) => n.textContent?.replace(/^\d\d:\d\d:\d\d/, ""))).toEqual([
    "Only the last 5,000 lines are kept.",
    ...Array.from({ length: 10 }, (_, i) => `line ${OUTPUT_LIMIT + i}`),
  ]);
  expect(panel.querySelectorAll("[data-testid=output-line]")).toHaveLength(OUTPUT_LIMIT);
});
