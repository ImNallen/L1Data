// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useWorkspace } from "../../state/workspace";
import { bindToFile } from "../../testing/notebooks";
import { dropEditor } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { notebookDirty } from "./notebookDirty";
import { ParamsStrip } from "./ParamsStrip";

const ws = () => useWorkspace.getState();
let id: string;
const params = () => ws().notebooks[id]?.params;

beforeEach(() => {
  vi.stubGlobal("window", { ...window, crypto });
  const cell = openSqlTab({ db: { connectionId: "c1", database: "shop" }, text: "select :x::int" });
  id = Object.values(ws().notebooks).find((n) => n.cells.includes(cell))!.id;
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  for (const tab of Object.keys(ws().tabs)) dropEditor(tab);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

it("shows nothing until the notebook holds a value", () => {
  render(<ParamsStrip id={id} />);
  expect(screen.queryByTestId("notebook-params")).toBeNull();
  act(() => ws().mergeParams(id, [{ name: ":x", value: "41" }]));
  expect(screen.getByTestId("notebook-param-value-:x")).toHaveProperty("value", "41");
});

it("edits a value as it is typed, toggles NULL, removes a name, and makes a file notebook unsaved", async () => {
  const user = userEvent.setup();
  ws().mergeParams(id, [
    { name: ":x", value: "41" },
    { name: "@Y", value: null },
  ]);
  bindToFile(id);
  render(<ParamsStrip id={id} />);
  expect(notebookDirty(id)).toBe(false);
  expect(screen.getByTestId("notebook-param-value-@Y")).toHaveProperty("disabled", true);
  await user.type(screen.getByTestId("notebook-param-value-:x"), "2");
  expect([params()?.[0], notebookDirty(id)]).toEqual([{ name: ":x", value: "412" }, true]);
  await user.click(screen.getByTestId("notebook-param-null-:x"));
  await user.click(screen.getByTestId("notebook-param-null-@Y"));
  expect(params()).toEqual([
    { name: ":x", value: null },
    { name: "@Y", value: "" },
  ]);
  await user.click(screen.getByTestId("notebook-param-remove-:x"));
  expect(params()).toEqual([{ name: "@Y", value: "" }]);
  await user.click(screen.getByTestId("notebook-param-remove-@Y"));
  expect(screen.queryByTestId("notebook-params")).toBeNull();
});
