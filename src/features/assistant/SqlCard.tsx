import { CheckIcon, CopyIcon, FileCode2Icon, PlayIcon, PlusIcon, SquareIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { DatabaseKind, DatabaseRef } from "../../ipc/bindings";
import { useWorkspace } from "../../state/workspace";
import { IconButton, Spinner } from "../../ui/primitives";
import { openInNewTab } from "./assistantActions";
import { InlineQueryRows, useInlineQuery } from "./InlineQuery";
import { SqlHighlight } from "./SqlHighlight";

export function SqlCard({
  sql,
  relations,
  vetted,
  complete,
  db,
  databaseKind = "Postgres",
}: {
  sql: string;
  relations: string[];
  vetted: boolean;
  complete: boolean;
  db: DatabaseRef | null;
  databaseKind?: DatabaseKind;
}) {
  const [copied, setCopied] = useState(false);
  const query = useInlineQuery(sql, db);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(sql);
      setCopied(true);
    } catch (e) {
      useWorkspace.getState().notify(`Could not copy: ${String(e)}`);
    }
  };
  return (
    <div data-testid="sql-card" className="chat-card">
      <div className="flex h-8 items-center gap-2 pr-1 pl-3">
        <FileCode2Icon size={14} className="shrink-0 text-muted" />
        <span className="min-w-0 flex-1 truncate text-ui-12 font-medium" title={relations.join(", ")}>
          SQL query
        </span>
        {relations.length > 0 && (
          <span data-testid="sql-card-uses" className="sr-only">
            Uses {relations.join(", ")}
          </span>
        )}
        <IconButton
          data-testid="sql-card-copy"
          aria-label="Copy SQL"
          title={copied ? "Copied" : "Copy SQL"}
          className="size-6"
          disabled={!complete}
          onClick={() => void copy()}
        >
          {copied ? <CheckIcon size={13} /> : <CopyIcon size={13} />}
        </IconButton>
      </div>
      <SqlHighlight sql={sql} databaseKind={databaseKind} className="chat-sql px-3 py-2" />
      <div className="flex min-h-10 items-center gap-2 px-3 py-1.5">
        {query.busy ? (
          <button type="button" data-testid="sql-stop" className="chat-button" onClick={query.stop}>
            <SquareIcon size={12} />
            Stop
          </button>
        ) : vetted ? (
          <button type="button" data-testid="sql-run-here" className="chat-button" disabled={!complete || !db} onClick={() => void query.run()}>
            <PlayIcon size={12} />
            {query.result ? "Run again" : "Run here"}
          </button>
        ) : (
          <button type="button" data-testid="sql-open-new-tab" className="chat-button" disabled={!complete} onClick={() => openInNewTab(sql, db)}>
            Add to cell
          </button>
        )}
        <div className="flex-1" />
        <IconButton
          data-testid="sql-add-cell"
          aria-label="Add SQL to a notebook cell"
          title="Add SQL to a notebook cell"
          disabled={!complete}
          onClick={() => openInNewTab(sql, db)}
        >
          <PlusIcon size={14} />
        </IconButton>
      </div>
      {(query.busy || query.summary || query.error) && (
        <div className="flex items-center gap-1.5 px-3 pb-2 text-ui-11 text-muted" role="status">
          {query.busy ? <Spinner /> : query.summary?.tone === "ok" ? <CheckIcon size={12} className="text-green" /> : null}
          <span className={query.error || query.summary?.tone === "error" ? "text-danger" : undefined}>
            {query.error ?? (query.busy ? "Running query…" : query.summary?.text)}
          </span>
        </div>
      )}
      {query.result && !query.error && <InlineQueryRows result={query.result} />}
    </div>
  );
}
