import { CheckIcon, ShieldCheckIcon, ChevronRightIcon, CircleOffIcon, DatabaseSearchIcon, PlayIcon, TableIcon, WrenchIcon } from "lucide-react";
import { memo, useId, useState, type ReactNode } from "react";
import type { ApprovalId, ApprovalReason, ChatId, DatabaseKind, DatabaseRef, Decision, ToolResult, ToolStatus, ToolUse } from "../../ipc/bindings";
import { useWorkspace } from "../../state/workspace";
import { toolLine } from "../../state/transcript";
import { Button, cx, Spinner } from "../../ui/primitives";
import { answerApproval, openInNewTab } from "./assistantActions";
import { InlineRows } from "./InlineQuery";
import { SqlHighlight } from "./SqlHighlight";

type RowsResult = Extract<ToolResult, { kind: "Rows" }>;

const TOOL_ICONS = { DescribeTables: TableIcon, SearchCatalog: DatabaseSearchIcon, RunQuery: PlayIcon, Other: WrenchIcon } satisfies Record<
  ToolUse["kind"],
  unknown
>;

const PREVIEW_ROWS = 8;

function RowsPreview({ rows, sql, db }: { rows: RowsResult; sql: string | null; db: DatabaseRef | null }) {
  const shown = rows.rows.slice(0, PREVIEW_ROWS);
  const hidden = rows.rows.length - shown.length;
  return (
    <div className="chat-card mt-2">
      <InlineRows columns={rows.columns} rows={shown} />
      <div className="flex items-center gap-2 border-t border-border px-2 py-1">
        <span className="flex-1 text-ui-11.5 text-dim">
          {hidden > 0 || rows.more ? `First ${shown.length} rows of what the assistant saw` : "Every row the assistant saw"}
        </span>
        {sql !== null && (
          <Button data-testid="tool-open-in-tab" variant="ghost" className="h-6 px-2 text-ui-12" onClick={() => openInNewTab(sql, db)}>
            Open in tab
          </Button>
        )}
      </div>
    </div>
  );
}

function ApprovalCard({
  chatId,
  testId,
  tool,
  approval,
  reason,
  databaseKind,
  db,
}: {
  db: DatabaseRef | null;
  chatId: ChatId;
  testId: string;
  tool: ToolUse;
  approval: ApprovalId;
  reason: ApprovalReason;
  databaseKind: DatabaseKind;
}) {
  const [answered, setAnswered] = useState(false);
  const config = useWorkspace((s) => (db ? s.configs[db.connectionId] : undefined));
  const question = useId();
  const answer = (decision: Decision) => {
    setAnswered(true);
    void answerApproval(chatId, approval, decision);
  };
  return (
    <div data-testid={testId} role="group" aria-labelledby={question} className="chat-card flex flex-col gap-2.5 p-3">
      <div className="flex items-center gap-2 text-ui-12 font-medium">
        <ShieldCheckIcon size={14} className="text-amber" />
        Approval required
      </div>
      {db && (
        <p className="text-ui-11 text-muted">
          {config?.name ?? db.connectionId} / {db.database}
        </p>
      )}
      <p id={question} className="text-ui-12.5 font-medium text-fg">
        {tool.kind === "RunQuery" ? "Run this query?" : "Allow this tool call?"}
      </p>
      {tool.kind === "RunQuery" && (
        <>
          {tool.purpose && <p className="text-ui-12.5 text-muted">{tool.purpose}</p>}
          <SqlHighlight sql={tool.sql} databaseKind={databaseKind} className="chat-sql rounded-md px-2.5 py-2" />
          {tool.relations.length > 0 && <p className="text-ui-12 text-dim">Uses {tool.relations.join(", ")}</p>}
        </>
      )}
      {reason.kind === "Unvetted" && <p className="text-ui-12 text-amber">Calls functions L1 Data cannot verify as read-only: {reason.functions.join(", ")}</p>}
      {config?.environment === "Production" && <p className="text-ui-11 text-muted">Runs a query on Production.</p>}
      <div className="flex items-center justify-between gap-2 pt-0.5">
        <button type="button" data-testid="approval-deny" className="chat-button chat-button-ghost" disabled={answered} onClick={() => answer("Deny")}>
          Reject
        </button>
        <button type="button" data-testid="approval-run" className="chat-button chat-button-primary" disabled={answered} onClick={() => answer("Run")}>
          Approve once
        </button>
      </div>
    </div>
  );
}

function StatusIcon({ tool, status }: { tool: ToolUse; status: ToolStatus }): ReactNode {
  if (status.kind === "Running") return <Spinner />;
  if (status.kind === "Denied" || status.kind === "Expired" || status.kind === "Failed") return <CircleOffIcon size={13} />;
  if (status.kind === "Done") return <CheckIcon size={13} className="text-green" />;
  const Icon = TOOL_ICONS[tool.kind];
  return <Icon size={13} />;
}

export const ToolCard = memo(function ToolCard({
  chatId,
  seq,
  tool,
  status,
  db,
  databaseKind,
}: {
  chatId: ChatId;
  seq: number;
  tool: ToolUse;
  status: ToolStatus;
  db: DatabaseRef | null;
  databaseKind: DatabaseKind;
}) {
  const [open, setOpen] = useState(true);
  const testId = `chat-tool-${seq}`;
  if (status.kind === "AwaitingApproval")
    return <ApprovalCard chatId={chatId} testId={testId} tool={tool} approval={status.approval} reason={status.reason} databaseKind={databaseKind} db={db} />;
  const rows = status.kind === "Done" && status.result.kind === "Rows" ? status.result : null;
  const line = toolLine(tool, status);
  return (
    <div data-testid={testId}>
      <button
        type="button"
        disabled={!rows}
        aria-expanded={rows ? open : undefined}
        title={tool.kind === "RunQuery" ? tool.purpose || line : line}
        onClick={() => setOpen((o) => !o)}
        className={cx("flex w-full min-w-0 items-center gap-2 rounded-md text-left text-ui-12.5 text-dim", rows && "hover:text-muted")}
      >
        <span className="grid w-3.5 shrink-0 place-items-center">
          <StatusIcon tool={tool} status={status} />
        </span>
        <span className="min-w-0 truncate">{line}</span>
        {rows && <ChevronRightIcon size={12} className={cx("shrink-0 transition-transform", open && "rotate-90")} />}
      </button>
      {open && rows && <RowsPreview rows={rows} sql={tool.kind === "RunQuery" ? tool.sql : null} db={db} />}
    </div>
  );
});
