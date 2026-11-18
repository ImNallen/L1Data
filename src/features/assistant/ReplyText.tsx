import { createContext, memo, useContext } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { DatabaseKind, DatabaseRef, ReplyBlock } from "../../ipc/bindings";
import { SqlCard } from "./SqlCard";
import { SqlHighlight } from "./SqlHighlight";

const REMARK = [remarkGfm];

const InPre = createContext(false);

const COMPONENTS: Components = {
  a: ({ children }) => <span className="text-fg">{children}</span>,
  img: ({ alt }) => <span>{alt}</span>,
  pre: ({ children }) => (
    <pre className="selectable overflow-x-auto rounded-lg border border-control px-3 py-2.5 font-mono text-code-sm leading-[1.6] text-(color:--code)">
      <InPre.Provider value>{children}</InPre.Provider>
    </pre>
  ),
  code: function Code({ children }) {
    if (useContext(InPre)) return <code>{children}</code>;
    return <code className="rounded-[4px] bg-pill px-1 py-px font-mono text-code-sm text-fg">{children}</code>;
  },
  p: ({ children }) => <p>{children}</p>,
  ul: ({ children }) => <ul className="list-disc space-y-1 pl-5">{children}</ul>,
  ol: ({ children }) => <ol className="list-decimal space-y-1 pl-5">{children}</ol>,
  h1: ({ children }) => <h3 className="text-ui-14 font-semibold">{children}</h3>,
  h2: ({ children }) => <h3 className="text-ui-14 font-semibold">{children}</h3>,
  h3: ({ children }) => <h4 className="text-ui-13 font-semibold">{children}</h4>,
  h4: ({ children }) => <h4 className="text-ui-13 font-semibold">{children}</h4>,
  blockquote: ({ children }) => <blockquote className="border-l-2 border-control pl-3 text-muted">{children}</blockquote>,
  hr: () => <hr className="border-border" />,
  table: ({ children }) => (
    <div className="overflow-x-auto rounded-lg border border-control">
      <table className="w-full border-collapse text-ui-12.5 tabular-nums">{children}</table>
    </div>
  ),
  th: ({ children, style }) => (
    <th style={style} className="border-b border-control px-2.5 py-1.5 text-left font-medium text-muted">
      {children}
    </th>
  ),
  td: ({ children, style }) => (
    <td style={style} className="border-t border-grid px-2.5 py-1.5">
      {children}
    </td>
  ),
};

const MarkdownBlock = memo(function MarkdownBlock({ text }: { text: string }) {
  return (
    <div className="selectable flex flex-col gap-2.5 text-ui-13 leading-relaxed text-fg">
      <Markdown remarkPlugins={REMARK} skipHtml components={COMPONENTS}>
        {text}
      </Markdown>
    </div>
  );
});

export function ReplyText({
  blocks,
  db,
  testId,
  databaseKind = "Postgres",
}: {
  blocks: ReplyBlock[];
  db: DatabaseRef | null;
  testId: string;
  databaseKind?: DatabaseKind;
}) {
  return (
    <div data-testid={testId} className="flex flex-col gap-3">
      {blocks.map((b, i) =>
        b.kind === "Markdown" ? (
          <MarkdownBlock key={i} text={b.text} />
        ) : b.kind === "Replacement" ? (
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
