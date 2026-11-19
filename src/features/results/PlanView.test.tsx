// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { commands, type Plan, type PlanNode } from "../../ipc/bindings";
import type { RowResult } from "../../state/status";
import { PlanView } from "./PlanView";

afterEach(() => {
  cleanup();
  performance.clearMarks("plan-paint");
  vi.restoreAllMocks();
});

function node(patch: Partial<PlanNode> & Pick<PlanNode, "operation">): PlanNode {
  return {
    detail: "",
    startupCost: 0,
    totalCost: 1,
    planRows: 1,
    actualRows: 1,
    loops: 1,
    totalTimeMs: 10,
    selfTimeMs: 4,
    diverges: false,
    buffers: null,
    properties: [],
    children: [],
    ...patch,
  };
}

const plan: Plan = {
  statements: [
    {
      planningTimeMs: 0.12,
      executionTimeMs: 1.5,
      statementText: "",
      properties: [],
      root: node({
        operation: "Hash Join",
        detail: "on public.orders o",
        selfTimeMs: 4,
        properties: [{ name: "Hash Cond", value: "(o.id = c.id)" }],
        children: [
          node({
            operation: "Seq Scan",
            detail: "on public.orders o",
            totalTimeMs: 6,
            selfTimeMs: 6,
            diverges: true,
            properties: [{ name: "Filter", value: "(n > 1)" }],
          }),
        ],
      }),
    },
  ],
};

const result: RowResult = {
  index: 0,
  plan: true,
  state: "Complete",
  meta: {
    databaseKind: "Postgres",
    ref: { tabId: "t1", runId: 1, index: 0 },
    db: { connectionId: "c1", database: "shop" },
    columns: [],
    rowCount: 1,
    rowsSeen: 1,
    truncated: null,
    elapsedMs: 38,
    allRows: { kind: "Refused", reason: "Explain is not re-run." },
    repeatedRead: null,
  },
};

function focused() {
  return screen.getAllByRole("treeitem").find((row) => row.getAttribute("data-focused") === "true");
}

it("renders the plan tree, moves focus from the keyboard, and keeps the detail on the focused node", async () => {
  const intoView = vi.fn();
  HTMLElement.prototype.scrollIntoView = intoView;
  vi.spyOn(commands, "resultPlan").mockResolvedValue({ status: "ok", data: plan });
  const user = userEvent.setup();
  render(<PlanView result={result} />);

  expect((await screen.findByTestId("plan-node-0")).textContent).toContain("Hash Join");
  expect(screen.queryByTestId("plan-statement-0")).toBeNull();
  expect(screen.getByTestId("plan-header").textContent).toContain("Planning 0.12 ms");
  expect(screen.getByTestId("plan-header").textContent).toContain("Execution 1.5 ms");
  expect(screen.getAllByTestId("plan-diverges")).toHaveLength(1);
  expect(screen.getByTestId("plan-node-1").textContent).toContain("actual 1");
  expect(screen.getByTestId("plan-node-1").textContent).toContain("10×");
  expect(screen.getByTestId("plan-node-0").textContent).not.toContain("10×");
  expect(focused()).toBe(screen.getByTestId("plan-node-0"));
  expect(screen.getByTestId("plan-detail").textContent).toContain("(o.id = c.id)");
  await waitFor(() => expect(performance.getEntriesByName("plan-paint")).toHaveLength(1));

  screen.getByRole("tree").focus();
  await user.keyboard("{ArrowDown}");
  expect(focused()).toBe(screen.getByTestId("plan-node-1"));
  expect([screen.getByRole("tree").getAttribute("aria-activedescendant"), screen.getByRole("treeitem", { selected: true })]).toEqual([
    screen.getByTestId("plan-node-1").id,
    screen.getByTestId("plan-node-1"),
  ]);
  expect(screen.getByTestId("plan-detail").textContent).toContain("(n > 1)");
  expect(screen.getByTestId("plan-detail").textContent).not.toContain("(o.id = c.id)");

  await user.keyboard("{Home}");
  expect(focused()).toBe(screen.getByTestId("plan-node-0"));
  await user.keyboard("{End}");
  expect(focused()).toBe(screen.getByTestId("plan-node-1"));
  expect(performance.getEntriesByName("plan-paint")).toHaveLength(1);
  expect(intoView).toHaveBeenCalled();
});

it("shows each statement above its tree and moves across both roots", async () => {
  const two: Plan = {
    statements: [
      {
        planningTimeMs: null,
        executionTimeMs: null,
        statementText: "SELECT 1",
        properties: [],
        root: node({ operation: "Constant Scan", children: [node({ operation: "Filter" })] }),
      },
      {
        planningTimeMs: null,
        executionTimeMs: null,
        statementText: "SELECT 2",
        properties: [],
        root: node({ operation: "Clustered Index Scan", totalTimeMs: null, selfTimeMs: null, actualRows: null }),
      },
    ],
  };
  vi.spyOn(commands, "resultPlan").mockResolvedValue({ status: "ok", data: two });
  const user = userEvent.setup();
  render(<PlanView result={result} />);

  expect((await screen.findByTestId("plan-statement-0")).textContent).toBe("SELECT 1");
  expect(screen.getByTestId("plan-statement-1").textContent).toBe("SELECT 2");
  expect(screen.getByTestId("plan-statement-0").getAttribute("role")).toBeNull();
  expect(screen.getAllByRole("treeitem")).toHaveLength(3);
  screen.getByRole("tree").focus();
  await user.keyboard("{End}");
  expect(focused()?.textContent).toContain("Clustered Index Scan");
  expect(screen.getByTestId("plan-header").textContent).not.toContain("Planning");
});

it("shows a plan read error and does not mark a paint", async () => {
  vi.spyOn(commands, "resultPlan").mockResolvedValue({ status: "error", error: { kind: "NotAPlan", what: "The result is not a plan." } });
  render(<PlanView result={result} />);
  expect((await screen.findByTestId("plan-error")).textContent).toBe("The result is not a plan.");
  expect(performance.getEntriesByName("plan-paint")).toHaveLength(0);
});

it("uses one message when the result cannot be read", async () => {
  vi.spyOn(commands, "resultPlan").mockResolvedValue({ status: "error", error: { kind: "Gone" } });
  render(<PlanView result={result} />);
  expect((await screen.findByTestId("plan-error")).textContent).toBe("The plan could not be read.");
});
