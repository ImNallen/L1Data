// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { ParamValue } from "../../ipc/bindings";
import { newNotebook } from "../../state/notebooks";
import { useWorkspace } from "../../state/workspace";
import { ParamsDialog } from "./ParamsDialog";
import { finishParams, resolveParams } from "./paramsState";

const params = () => useWorkspace.getState().notebooks.n?.params;

function notebook(values: ParamValue[]): void {
  useWorkspace.setState({ notebooks: { n: { ...newNotebook([], null), id: "n", params: values } } });
}

beforeEach(() => notebook([]));

afterEach(() => {
  cleanup();
  finishParams(null);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

it("asks only for names the notebook has no value for, each empty, keeps text and NULL distinct, and merges the answer", async () => {
  const user = userEvent.setup();
  notebook([{ name: ":known", value: "1" }]);
  render(<ParamsDialog />);
  let answer: ReturnType<typeof resolveParams> | undefined;
  act(() => {
    answer = resolveParams("n", [{ name: ":name" }, { name: ":known" }, { name: "$1" }], "dev / shop", () => true);
  });
  expect(await screen.findByTestId("params-dialog")).toBeTruthy();
  expect(screen.queryByTestId("param-value-:known")).toBeNull();
  expect(screen.getByTestId("param-value-:name")).toHaveProperty("value", "");
  expect(screen.getByTestId("param-value-:name")).toBe(document.activeElement);
  await user.type(screen.getByTestId("param-value-:name"), "O'Reilly\\literal");
  await user.click(screen.getByTestId("param-null-$1"));
  await user.click(screen.getByTestId("params-run"));
  const merged = [
    { name: ":known", value: "1" },
    { name: ":name", value: "O'Reilly\\literal" },
    { name: "$1", value: null },
  ];
  await expect(answer).resolves.toEqual(merged);
  expect(params()).toEqual(merged);
});

it("resolves the notebook's list without asking once every name has a value", async () => {
  notebook([{ name: ":id", value: null }]);
  render(<ParamsDialog />);
  await expect(resolveParams("n", [{ name: ":id" }], "dev / shop", () => true)).resolves.toEqual([{ name: ":id", value: null }]);
  expect(screen.queryByTestId("params-dialog")).toBeNull();
});

it("cancels on Escape and merges nothing", async () => {
  render(<ParamsDialog />);
  let answer: ReturnType<typeof resolveParams> | undefined;
  act(() => {
    answer = resolveParams("n", [{ name: ":id" }], "dev / shop", () => true);
  });
  await screen.findByTestId("params-dialog");
  await userEvent.setup().keyboard("{Escape}");
  await expect(answer).resolves.toBeNull();
  expect(params()).toEqual([]);
});

it("remounts a superseded prompt for the same name empty", async () => {
  const user = userEvent.setup();
  render(<ParamsDialog />);
  let first: ReturnType<typeof resolveParams> | undefined;
  let second: ReturnType<typeof resolveParams> | undefined;
  act(() => {
    first = resolveParams("n", [{ name: ":id" }], "dev / shop", () => true);
  });
  await user.type(await screen.findByTestId("param-value-:id"), "unsent");
  act(() => {
    second = resolveParams("n", [{ name: ":id" }], "dev / shop", () => true);
  });
  await expect(first).resolves.toBeNull();
  expect(screen.getByTestId("param-value-:id")).toHaveProperty("value", "");
  await user.click(screen.getByTestId("params-run"));
  await expect(second).resolves.toEqual([{ name: ":id", value: "" }]);
});
