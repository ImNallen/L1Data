import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { ReplyText } from "./ReplyText";

it("draws a link as its text with no href and an image as its alt text with no img", () => {
  const html = renderToStaticMarkup(
    <ReplyText
      blocks={[{ kind: "Markdown", text: "See [orders](https://example.com/x) and ![growth chart](https://example.com/c.png)." }]}
      db={null}
      testId="reply"
    />,
  );
  expect(html).toContain('<span class="text-fg">orders</span>');
  expect(html).toContain("<span>growth chart</span>");
  expect(html.replace(/<[^>]+>/g, "")).toBe("See orders and growth chart.");
  expect([html.includes("<a "), html.includes("href="), html.includes("<img")]).toEqual([false, false, false]);
});

it("shows a validated replacement as code without query actions", () => {
  const html = renderToStaticMarkup(<ReplyText blocks={[{ kind: "Replacement", sql: "SELECT 2;" }]} db={null} testId="reply" />);
  expect(html.replace(/<[^>]+>/g, "")).toBe("SELECT 2;");
  expect(html).not.toContain("<button");
});
