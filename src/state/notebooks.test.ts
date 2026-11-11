import { expect, it } from "vitest";
import { adjacentCell, moveCell } from "./notebooks";

it("finds the cell before and after, and none past either end or for a cell not in the list", () => {
  const cells = ["a", "b", "c"];
  expect(adjacentCell(cells, "b", -1)).toBe("a");
  expect(adjacentCell(cells, "b", 1)).toBe("c");
  expect(adjacentCell(cells, "a", -1)).toBeUndefined();
  expect(adjacentCell(cells, "c", 1)).toBeUndefined();
  expect(adjacentCell(cells, "z", 1)).toBeUndefined();
});

it("moves a cell to an index past its neighbours, either way, and clamps an index past either end", () => {
  const cells = ["a", "b", "c", "d"];
  expect(moveCell(cells, "a", 2)).toEqual(["b", "c", "a", "d"]);
  expect(moveCell(cells, "d", 0)).toEqual(["d", "a", "b", "c"]);
  expect(moveCell(cells, "b", 2)).toEqual(["a", "c", "b", "d"]);
  expect(moveCell(cells, "c", 9)).toEqual(["a", "b", "d", "c"]);
  expect(moveCell(cells, "b", -3)).toEqual(["b", "a", "c", "d"]);
  expect(cells).toEqual(["a", "b", "c", "d"]);
});

it("returns the same cells when the cell is already at the index, clamped there, or not in the list", () => {
  const cells = ["a", "b", "c"];
  expect(moveCell(cells, "b", 1)).toBe(cells);
  expect(moveCell(cells, "a", -1)).toBe(cells);
  expect(moveCell(cells, "c", 3)).toBe(cells);
  expect(moveCell(cells, "z", 0)).toBe(cells);
});
