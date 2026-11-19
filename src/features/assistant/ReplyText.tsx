import { Markdown } from "@pierre/words/react";
import { memo } from "react";
import type { DatabaseKind, DatabaseRef, ReplyBlock } from "../../ipc/bindings";
import { SqlCard } from "./SqlCard";
import { SqlHighlight } from "./SqlHighlight";

const linkAsText = (html: string) => html.replace(/^<a\b[^>]*>|<\/a>$/g, "");
// Words passes each hook the element's HTML with attribute values escaped, so the alt text is safe to reuse.
const imageAsAltText = (html: string) => `<span>${/\balt="([^"]*)"/.exec(html)?.[1] ?? ""}</span>`;
const framedTable = (html: string) => `<div data-markdown="table-frame">${html}</div>`;
const COMPONENTS = { a: linkAsText, img: imageAsAltText, table: framedTable };

const MarkdownBlock = memo(function MarkdownBlock({ text, streaming }: { text: string; streaming: boolean }) {
  return (
    <Markdown className="reply-markdown selectable" isAnimating={streaming} animated controls={false} lineNumbers={false} components={COMPONENTS}>
      {text}
    </Markdown>
  );
});

export function ReplyText({
  blocks,
  db,
  testId,
  databaseKind = "Postgres",
  cards = true,
  streaming,
}: {
  blocks: ReplyBlock[];
  db: DatabaseRef | null;
  testId: string;
  databaseKind?: DatabaseKind;
  cards?: boolean;
  streaming: boolean;
}) {
  return (
    <div data-testid={testId} className="flex flex-col gap-3">
      {blocks.map((b, i) =>
        b.kind === "Markdown" ? (
          <MarkdownBlock key={i} text={b.text} streaming={streaming && i === blocks.length - 1} />
        ) : b.kind === "Replacement" || !cards ? (
          <div key={i} className="max-h-80 overflow-auto rounded-md border border-control px-3 py-2">
            <SqlHighlight sql={b.sql} databaseKind={databaseKind} />
          </div>
        ) : (
          <SqlCard key={i} sql={b.sql} relations={b.relations} vetted={b.vetted} complete={b.complete} db={db} databaseKind={databaseKind} />
        ),
      )}
    </div>
  );
}
