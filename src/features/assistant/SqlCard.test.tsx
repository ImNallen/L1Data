import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { SqlCard } from "./SqlCard";
import { sqlSpans } from "./SqlHighlight";

const SQL = "SELECT c.region, count(*) AS orders\nFROM orders o JOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\nGROUP BY c.region;";
const DB = { connectionId: "c1", database: "shop" };

const card = (props: { vetted?: boolean; complete: boolean }) =>
  renderToStaticMarkup(<SqlCard sql={SQL} relations={["orders", "customers"]} vetted={props.vetted ?? true} complete={props.complete} db={DB} />);
const button = (html: string, id: string) => new RegExp(`<button[^>]*data-testid="${id}"[^>]*>`).exec(html)?.[0] ?? null;
const disabled = (html: string, ids: string[]) => ids.map((id) => button(html, id)?.includes('disabled=""') ?? null);
const ACTIONS = ["sql-run-here", "sql-add-cell", "sql-card-copy"];

it("names the relations the SQL uses and holds every action until the block is complete", () => {
  const streaming = card({ complete: false });
  expect(/data-testid="sql-card-uses"[^>]*>(.*?)<\/span>/.exec(streaming)?.[1]).toBe("Uses orders, customers");
  expect(disabled(streaming, ACTIONS)).toEqual([true, true, true]);
  expect(disabled(card({ complete: true }), ACTIONS)).toEqual([false, false, false]);
});

it("offers Open in new tab instead of Run when the engine did not vet the SQL", () => {
  const html = card({ vetted: false, complete: true });
  expect([button(html, "sql-run-here"), disabled(html, ["sql-open-new-tab"])]).toEqual([null, [false]]);
  expect(html).toContain(">Add to cell</button>");
});

it("colors SQL with the editor's token classes and draws a keyword-named call as a plain name", () => {
  const colored = sqlSpans("SELECT count(*) FROM orders WHERE status = 'paid' LIMIT 10; -- top")
    .filter((s) => s.className !== "" && s.text.trim() !== "")
    .map((s) => `${s.className}:${s.text}`);
  expect(colored).toEqual([
    "text-keyword:SELECT",
    "text-(color:--code-punct):(",
    "text-(color:--code-punct):*",
    "text-(color:--code-punct):)",
    "text-keyword:FROM",
    "text-(color:--code):orders",
    "text-keyword:WHERE",
    "text-(color:--code):status",
    "text-(color:--code-punct):=",
    "text-green:'paid'",
    "text-keyword:LIMIT",
    "text-amber:10",
    "text-dim:-- top",
  ]);
  expect(
    sqlSpans("select 1;\n")
      .map((s) => s.text)
      .join(""),
  ).toBe("select 1;\n");
});
