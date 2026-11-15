import { expect, it } from "vitest";
import type { ParamValue } from "../ipc/bindings";
import { mergedParams, missingParams, paramsKey, withoutParam } from "./notebookParams";

const list: ParamValue[] = [
  { name: ":region", value: "EU" },
  { name: "$1", value: null },
];

it("keys a list by its names, values, and order, and tells NULL from empty text", () => {
  expect(paramsKey(list)).toBe(paramsKey([...list]));
  expect(paramsKey(list)).not.toBe(paramsKey([...list].reverse()));
  expect(paramsKey([{ name: ":x", value: null }])).not.toBe(paramsKey([{ name: ":x", value: "" }]));
  expect(paramsKey([{ name: ":x", value: "null" }])).not.toBe(paramsKey([{ name: ":x", value: null }]));
});

it("sets values by name, keeps each known name in its place, adds new names last, and returns the same list for no change", () => {
  expect(
    mergedParams(list, [
      { name: "@new", value: "1" },
      { name: "$1", value: "7" },
    ]),
  ).toEqual([
    { name: ":region", value: "EU" },
    { name: "$1", value: "7" },
    { name: "@new", value: "1" },
  ]);
  expect(mergedParams(list, [{ name: ":region", value: "EU" }])).toBe(list);
  expect(mergedParams(list, [])).toBe(list);
});

it("removes a name, and returns the same list for a name it does not hold", () => {
  expect(withoutParam(list, ":region")).toEqual([{ name: "$1", value: null }]);
  expect(withoutParam(list, ":other")).toBe(list);
});

it("names each name with no value once, in the order found, and counts NULL as a value", () => {
  expect(missingParams([{ name: ":a" }, { name: "$1" }, { name: ":region" }, { name: ":a" }, { name: "@b" }], list)).toEqual([{ name: ":a" }, { name: "@b" }]);
});
