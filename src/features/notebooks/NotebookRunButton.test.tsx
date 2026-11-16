// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it } from "vitest";
import { useLive } from "../../state/live";
import { notebookOf } from "../../state/notebooks";
import { useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { fakeEngine, RUN_DB } from "../../testing/runEngine";
import { dropEditor } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { NotebookRunButton } from "./NotebookRunButton";

const ws = () => useWorkspace.getState();

beforeEach(() => {
  useWorkspace.setState({ configs: { prod: connectionConfig("prod", "dev") } });
});

afterEach(() => {
  cleanup();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

it("runs every cell, shows Stop with which cell runs, and stops on Stop", async () => {
  const user = userEvent.setup();
  const engine = fakeEngine({ outcomes: { "select pg_sleep(5)": "hold" } });
  const cells = ["select 1", "select pg_sleep(5)", "select 3"].map((text) => openSqlTab({ db: RUN_DB, text }));
  render(<NotebookRunButton id={notebookOf(ws().notebooks, cells[0]!)!.id} environment={null} />);

  await user.click(screen.getByTestId("run-all-btn"));
  await waitFor(() => expect(screen.getByTestId("run-progress").textContent).toBe("2 of 3"));
  expect(screen.getByTestId("stop-run-btn").textContent).toContain("Stop");
  await user.click(screen.getByTestId("stop-run-btn"));
  await waitFor(() => expect(screen.getByTestId("run-all-btn")).toBeTruthy());
  expect(engine.texts()).toEqual(["select 1", "select pg_sleep(5)"]);
  expect(ws().toast?.text).toBe("Stopped. 2 cells did not run.");
});
