import { describe, expect, it } from "vitest";
import { staleness, stalenessText } from "./references";

const cell = (name: string, text: string) => ({ name, text });

describe("staleness", () => {
  it("is empty while every inlined cell still has the text the run used", () => {
    expect(staleness([cell("a", "select 1")], [cell("a", "select 1"), cell("b", "select * from {{a}}")])).toEqual([]);
  });

  it("names a cell whose text changed, a cell that is gone, and a name two cells now share", () => {
    const sources = [cell("a", "select 1"), cell("b", "select 2"), cell("c", "select 3")];
    const cells = [cell("a", "select 10"), cell("c", "select 3"), cell("c", "select 4")];
    expect(staleness(sources, cells)).toEqual([
      { name: "a", why: "changed" },
      { name: "b", why: "missing" },
      { name: "c", why: "ambiguous" },
    ]);
  });

  it("says why in one sentence per reason", () => {
    expect(
      stalenessText([
        { name: "a", why: "changed" },
        { name: "b", why: "missing" },
      ]),
    ).toBe("a changed since this cell ran. No cell is named b now.");
  });
});
