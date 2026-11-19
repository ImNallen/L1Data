import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import type { ReplyBlock } from "../../ipc/bindings";
import { ReplyText } from "./ReplyText";

const render = (blocks: ReplyBlock[], streaming = false) => renderToStaticMarkup(<ReplyText blocks={blocks} db={null} testId="reply" streaming={streaming} />);
const text = (html: string) => html.replace(/<[^>]+>|\n/g, "");

it("draws a link as its text with no href and an image as its alt text with no img", () => {
  const html = render([{ kind: "Markdown", text: "See [orders](https://example.com/x) and ![growth chart](https://example.com/c.png)." }]);
  expect(text(html)).toBe("See orders and growth chart.");
  expect([html.includes("<a "), html.includes("href="), html.includes("<img")]).toEqual([false, false, false]);
});

it("keeps markup characters in alt text as text", () => {
  const html = render([{ kind: "Markdown", text: '![<b>x</b> & "y"](https://example.com/c.png)' }]);
  expect(html).not.toContain("<b>");
  expect(text(html)).toBe("&lt;b&gt;x&lt;/b&gt; &amp; &quot;y&quot;");
});

it("shows a validated replacement as code without query actions", () => {
  const html = render([{ kind: "Replacement", sql: "SELECT 2;" }]);
  expect(text(html)).toBe("SELECT 2;");
  expect(html).not.toContain("<button");
});

it("completes unfinished Markdown in the block that is still streaming", () => {
  const html = render([{ kind: "Markdown", text: "Index **created_at and see [the d" }], true);
  expect(html).toContain("<strong");
  expect(text(html)).toBe("Index created_at and see the d");
});

it("leaves earlier blocks of a streaming reply as written", () => {
  const html = render(
    [
      { kind: "Markdown", text: "Index **created_at" },
      { kind: "Replacement", sql: "SELECT 1;" },
      { kind: "Markdown", text: "Then **status" },
    ],
    true,
  );
  expect(text(html)).toBe("Index **created_atSELECT 1;Then status");
});
