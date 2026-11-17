// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it } from "vitest";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useWorkspace, type Tab } from "../../state/workspace";
import { cellsState } from "../../testing/notebooks";
import { shopCatalog } from "../../testing/shopCatalog";
import { AddCellMenu } from "./AddCellMenu";
import { docOf, dropEditor } from "./editorStates";

const db = { connectionId: "p", database: "shop" };
const tab = (id: string, schema?: string): Tab => ({ id, title: `Cell ${id}`, console: { db, mode: "AutoCommit", ...(schema && { schema }) }, table: null });
const cells = () => Object.values(useWorkspace.getState().notebooks)[0]!.cells;

beforeEach(() => {
  useWorkspace.setState(cellsState([tab("a", "lab"), tab("b")], "a"));
  const view = buildCatalogView(shopCatalog("p", 1));
  useCatalog.setState({ views: { [dbKey(view.catalog.db)]: view } });
});

afterEach(() => {
  cleanup();
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
});

it("offers a SQL query and a schema map named by the database and the active cell's schema", async () => {
  const user = userEvent.setup();
  render(<AddCellMenu after="a" testId="add-cell" />);
  await user.click(screen.getByTestId("add-cell"));
  expect(screen.getAllByRole("menuitem").map((m) => m.textContent)).toEqual(["SQL query", "Schema mapshop · lab"]);
});

it("adds a SQL cell right after its place", async () => {
  const user = userEvent.setup();
  render(<AddCellMenu after="a" testId="add-cell" />);
  await user.click(screen.getByTestId("add-cell"));
  await user.click(screen.getByTestId("add-cell-sql"));
  const added = cells()[1]!;
  expect([cells().length, useWorkspace.getState().tabs[added]?.map, docOf(added)?.toString()]).toEqual([3, undefined, ""]);
});

it("adds a schema map cell right after its place, with no editor buffer", async () => {
  const user = userEvent.setup();
  render(<AddCellMenu after="a" testId="add-cell" />);
  await user.click(screen.getByTestId("add-cell"));
  await user.click(screen.getByTestId("add-cell-map"));
  const added = cells()[1]!;
  const t = useWorkspace.getState().tabs[added];
  expect([cells().length, t?.title, t?.console.schema, t?.map, docOf(added)]).toEqual([3, "Schema map", "lab", { columns: "All" }, undefined]);
});
