import { memo, useDeferredValue, useLayoutEffect, useRef, type ReactNode } from "react";
import type { OutputEntry, QueryError } from "../../ipc/bindings";
import { OUTPUT_LIMIT, type OutputLog } from "../../state/live";
import { cx } from "../../ui/primitives";
import { formatClock, formatElapsed, groupCount, pluralRows } from "./format";

export function ErrorBlock({ error, label }: { error: QueryError; label?: ReactNode }) {
  return (
    <>
      <div className="text-danger">
        {label}
        {error.message}
      </div>
      {error.detail && <div className="text-muted">{error.detail}</div>}
      {error.hint && <div className="text-muted">Hint: {error.hint}</div>}
      {error.context && <div className="whitespace-pre-wrap text-dim">Context: {error.context}</div>}
      {error.sqlstate && <div className="font-mono text-ui-11.5 text-dim">SQLSTATE {error.sqlstate}</div>}
    </>
  );
}

function Label({ className, children }: { className: string; children: ReactNode }) {
  return <span className={cx("mr-2", className)}>{children}</span>;
}

function LineBody({ body }: { body: OutputEntry["body"] }) {
  switch (body.kind) {
    case "Statement":
      return (
        <div className="text-fg">
          <Label className="text-dim">&gt;</Label>
          {body.text}
        </div>
      );
    case "Rows":
      return (
        <div className="text-dim">
          {pluralRows(body.rows)}
          {body.truncated && ", past the result limit"} · {formatElapsed(body.elapsedMs)}
        </div>
      );
    case "Command":
      return (
        <div className="text-dim">
          {body.tag} · {formatElapsed(body.elapsedMs)}
        </div>
      );
    case "Notice":
      return (
        <>
          <div className="text-fg">
            <Label className={body.severity === "WARNING" ? "text-amber" : "text-dim"}>{body.severity}:</Label>
            {body.message}
          </div>
          {body.detail && <div className="text-dim">{body.detail}</div>}
        </>
      );
    case "Error":
      return <ErrorBlock error={body.error} label={<Label className="text-danger">ERROR:</Label>} />;
    case "Info":
      return <div className="text-dim">{body.message}</div>;
    default: {
      const unreachable: never = body;
      return unreachable;
    }
  }
}

function severityOf(body: OutputEntry["body"]): string | undefined {
  if (body.kind === "Notice") return body.severity;
  if (body.kind === "Error") return body.error.severity;
  return undefined;
}

type LineProps = { time: string; runStart: boolean; kind?: OutputEntry["body"]["kind"]; severity?: string; children: ReactNode };

function Line({ time, runStart, kind, severity, children }: LineProps) {
  return (
    <div
      data-testid={kind && "output-line"}
      data-kind={kind}
      data-severity={severity}
      className={cx("flex gap-4 px-5 [contain-intrinsic-size:auto_1.25rem] [content-visibility:auto]", runStart && "mt-2 border-t border-grid pt-2")}
    >
      <span className="w-[8ch] shrink-0 text-dim">{time}</span>
      <div className="min-w-0 flex-1 whitespace-pre-wrap">{children}</div>
    </div>
  );
}

const OutputLine = memo(function OutputLine({ entry, runStart }: { entry: OutputEntry; runStart: boolean }) {
  return (
    <Line kind={entry.body.kind} severity={severityOf(entry.body)} time={formatClock(entry.atMs)} runStart={runStart}>
      <LineBody body={entry.body} />
    </Line>
  );
});

const LINES_PER_CHUNK = 100;

type ChunkProps = { start: number; lines: readonly OutputEntry[]; prevRunId: number | undefined };

const OutputChunk = memo(
  function OutputChunk({ start, lines, prevRunId }: ChunkProps) {
    return lines.map((entry, i) => {
      const prev = i > 0 ? lines[i - 1]!.runId : prevRunId;
      return <OutputLine key={start + i} entry={entry} runStart={prev !== undefined && prev !== entry.runId} />;
    });
  },
  (a, b) => a.start === b.start && a.prevRunId === b.prevRunId && a.lines.length === b.lines.length && a.lines.every((e, i) => e === b.lines[i]),
);

export const OutputPanel = memo(function OutputPanel({ log }: { log: OutputLog }) {
  const { entries, dropped } = useDeferredValue(log);
  const ref = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  useLayoutEffect(() => {
    const el = ref.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
  });
  const chunks = [];
  for (let from = 0; from < entries.length;) {
    const start = dropped + from;
    const to = Math.min(entries.length, from + LINES_PER_CHUNK - (start % LINES_PER_CHUNK));
    chunks.push(<OutputChunk key={Math.floor(start / LINES_PER_CHUNK)} start={start} lines={entries.slice(from, to)} prevRunId={entries[from - 1]?.runId} />);
    from = to;
  }

  return (
    <div
      ref={ref}
      data-testid="output-panel"
      className="selectable min-h-0 flex-1 overflow-auto bg-bg py-3 font-mono text-ui-12.5 leading-5"
      onScroll={(e) => {
        const el = e.currentTarget;
        atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 4;
      }}
    >
      {dropped > 0 && (
        <Line time="" runStart={false}>
          <div className="text-dim">Only the last {groupCount(OUTPUT_LIMIT)} lines are kept.</div>
        </Line>
      )}
      {chunks}
    </div>
  );
});
