import { expect, it } from "vitest";
import { adjacentCell } from "./notebooks";

it("finds the cell before and after, and none past either end or for a cell not in the list", () => {
  const cells = ["a", "b", "c"];
  expect(adjacentCell(cells, "b", -1)).toBe("a");
  expect(adjacentCell(cells, "b", 1)).toBe("c");
  expect(adjacentCell(cells, "a", -1)).toBeUndefined();
  expect(adjacentCell(cells, "c", 1)).toBeUndefined();
  expect(adjacentCell(cells, "z", 1)).toBeUndefined();
});
