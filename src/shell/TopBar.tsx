import * as Menu from "@radix-ui/react-dropdown-menu";
import { ChevronDownIcon, ListIndentIncreaseIcon, PlayIcon, SquareIcon } from "lucide-react";
import { ConnectionColorDot } from "../features/connections/connectionColors";
import {
  cancelActive,
  endTransaction,
  explainActive,
  explainSupported,
  formatActive,
  isBrowsing,
  runOrChoose,
  setTabMode,
  setTabTarget,
} from "../features/editor/runActions";
import type { ConnectionState, DatabaseRef, Environment, TransactionLoss, TxnMode } from "../ipc/bindings";
import { homeDb } from "../state/dbKey";
import { instanceOf, useLive } from "../state/live";
import { canCancel, connectionTone, hasUncommittedWork, isBusy } from "../state/status";
import { tabTarget, useWorkspace, type Tab } from "../state/workspace";
import { Button, CONNECTION_TONE_LABEL, Kbd, MenuContent, MenuItem, MenuLabel, StatusDot, cx } from "../ui/primitives";
import { SidebarToggle, WindowEdgeActions, windowEdgeInset, windowLeadingInset } from "./WindowEdgeActions";
import type { HostedWindow } from "./hostedWindow";

const MODE_LABEL: Record<TxnMode, string> = { AutoCommit: "Auto-commit", Manual: "Manual" };
const LOSS_LABEL: Record<TransactionLoss, { label: string; title: string } | null> = {
  None: null,
  RolledBack: { label: "rolled back", title: "Transaction rolled back: connection lost" },
  Unknown: { label: "session reset", title: "Session reset. Earlier commits remain committed; the last transaction outcome may be unknown." },
};

const TARGET_TRIGGER = "flex h-7 min-w-0 items-center gap-1 rounded-md px-1 text-ui-14 hover:bg-hover data-[state=open]:bg-hover";

/** The databases the tree shows, and the tab's own even when the tree hides it or the connection is down. */
export function databaseChoices(db: DatabaseRef, state: ConnectionState | undefined): string[] {
  const shown =
    instanceOf(state)
      ?.databases.filter((d) => d.shown)
      .map((d) => d.name) ?? [];
  return shown.includes(db.database) ? shown : [db.database, ...shown];
}

function Separator() {
  return <span className="mx-1.5 shrink-0 text-ui-14 text-faint">/</span>;
}

function DatabaseMenu({ tab, db }: { tab: Tab; db: DatabaseRef }) {
  const state = useLive((s) => s.connections[db.connectionId]);
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger data-testid="tab-database" title={db.database} className={cx(TARGET_TRIGGER, "text-fg")}>
        <span className="truncate">{db.database}</span>
        <ChevronDownIcon size={12} className="shrink-0 text-dim" />
      </Menu.Trigger>
      <MenuContent>
        <MenuLabel>Database for this tab</MenuLabel>
        {databaseChoices(db, state).map((name) => (
          <MenuItem
            key={name}
            testId={`tab-database-${name}`}
            checked={name === db.database}
            onSelect={() => void setTabTarget(tab.id, { connectionId: db.connectionId, database: name })}
          >
            {name}
          </MenuItem>
        ))}
      </MenuContent>
    </Menu.Root>
  );
}

function RunButton({ busy, environment, explainable }: { busy: boolean; environment: Environment | null; explainable: boolean }) {
  return (
    <span className="run-environment inline-flex" data-environment={environment ?? undefined}>
      <Button
        data-testid="run-btn"
        data-environment={environment ?? undefined}
        disabled={busy}
        variant="primary"
        title={`Run the selection or the marked statement (⌘↵). On a blank line, or with ⌘⇧↵, choose which statement to run.${environment === "Production" ? " This tab targets Production, so a run that writes asks first." : ""}`}
        className={cx("min-w-[104px] justify-center pr-2 pl-2.5 font-medium", explainable && "rounded-r-none")}
        onClick={() => void runOrChoose()}
      >
        <PlayIcon fill="currentColor" size={13} className="shrink-0 text-run-icon" />
        Run
        <Kbd tone="accent">⌘↵</Kbd>
      </Button>
      {explainable && (
        <Menu.Root modal={false}>
          <Menu.Trigger
            data-testid="explain-menu"
            disabled={busy}
            aria-label="Explain"
            className="inline-flex h-8 items-center rounded-r-lg border-l border-run-fg/25 bg-run px-1.5 text-run-fg hover:bg-run-hover focus-visible:outline-run-fg disabled:pointer-events-none disabled:opacity-40"
          >
            <ChevronDownIcon size={14} />
          </Menu.Trigger>
          <MenuContent align="end">
            <MenuItem testId="explain-item" onSelect={() => explainActive("Explain")}>
              <span className="flex-1">Explain</span>
              <Kbd tone="menu">⌘⌥↵</Kbd>
            </MenuItem>
            <MenuItem testId="explain-analyze-item" onSelect={() => explainActive("ExplainAnalyze")}>
              <span className="flex-1">Explain Analyze</span>
              <Kbd tone="menu">⌘⌥⇧↵</Kbd>
            </MenuItem>
          </MenuContent>
        </Menu.Root>
      )}
    </span>
  );
}

export function TopBar({ hosted }: { hosted: HostedWindow | null }) {
  const tab = useWorkspace((s) => (s.active ? s.tabs[s.active] : undefined));
  const configs = useWorkspace((s) => s.configs);
  const layout = useWorkspace((s) => s.layout);
  const db = tabTarget(tab);
  const connectionId = db?.connectionId ?? null;
  const liveConsole = useLive((s) => (tab ? s.consoles[tab.id] : undefined));
  const connections = useLive((s) => s.connections);
  const config = connectionId ? configs[connectionId] : undefined;
  const busy = isBusy(liveConsole);
  const cancellable = canCancel(liveConsole);
  const txn = liveConsole?.txn;
  const txnDepth = liveConsole?.txnDepth;
  const sqlServer = config?.databaseKind === "SqlServer";
  const loss = liveConsole?.session.kind === "Lost" ? LOSS_LABEL[liveConsole.session.transaction] : null;
  const environment = config?.environment ?? null;

  // The database sits in a zero-basis box that gets only the room left over, so it truncates before the connection
  // name. The top padding matches the bottom border, so the items stay level with the sidebar header and the traffic
  // lights.
  return (
    <header
      data-tauri-drag-region="deep"
      className={cx(
        "@container flex h-[52px] shrink-0 items-center gap-2 border-b-2 border-transparent pt-[2px]",
        windowEdgeInset(hosted),
        layout.sidebar ? "pl-4" : windowLeadingInset(hosted),
      )}
      data-testid="top-bar"
      data-environment={environment ?? undefined}
    >
      {!layout.sidebar && <SidebarToggle />}
      <div className="flex min-w-0 flex-1 items-center">
        {tab ? (
          <Menu.Root modal={false}>
            <Menu.Trigger data-testid="tab-connection" title={config?.name} className={cx(TARGET_TRIGGER, "-ml-1", config ? "text-fg" : "text-dim")}>
              <ConnectionColorDot color={config?.color ?? null} className="mr-0.5" />
              <span className="truncate">{config?.name ?? "No connection"}</span>
              <ChevronDownIcon size={12} className="shrink-0 text-dim" />
            </Menu.Trigger>
            <MenuContent>
              <MenuLabel>Connection for this tab</MenuLabel>
              {Object.values(configs).map((c) => {
                const tone = connectionTone(connections[c.id]);
                return (
                  <MenuItem key={c.id} checked={c.id === connectionId} onSelect={() => void setTabTarget(tab.id, homeDb(c))}>
                    <StatusDot tone={tone} label={CONNECTION_TONE_LABEL[tone]} />
                    <ConnectionColorDot color={c.color} />
                    {c.name}
                  </MenuItem>
                );
              })}
            </MenuContent>
          </Menu.Root>
        ) : (
          <span className="flex min-w-0 items-center gap-1.5 text-ui-14 text-dim">
            <ConnectionColorDot color={config?.color ?? null} />
            <span className="truncate">{config?.name ?? ""}</span>
          </span>
        )}
        {tab && db && config && (
          <div className="flex min-w-0 flex-1 items-center overflow-hidden">
            <Separator />
            <DatabaseMenu tab={tab} db={db} />
          </div>
        )}
      </div>

      {tab && <span aria-hidden className="h-5 w-px shrink-0 bg-border" />}
      {tab && (
        <Menu.Root modal={false}>
          <Menu.Trigger
            data-testid="tx-mode"
            title={
              loss?.title ?? (txn === "Open" ? "Transaction open. SQL Server does not report nesting depth or committability through the protocol." : undefined)
            }
            className="inline-flex h-7 items-center gap-1 rounded-md pr-1.5 pl-2 text-ui-12.5 text-muted hover:bg-hover hover:text-fg data-[state=open]:bg-hover data-[state=open]:text-fg"
          >
            {MODE_LABEL[tab.console.mode]}
            {txn && txn !== "Idle" && (
              <span className={txn === "Aborted" ? "text-danger" : "text-amber"}>
                {" "}
                · {txn === "Aborted" ? "aborted" : "in transaction"}
                {txnDepth !== null && txnDepth !== undefined && txnDepth > 1 ? ` (${txnDepth})` : ""}
              </span>
            )}
            {loss && <span className="text-danger"> · {loss.label}</span>}
            <ChevronDownIcon size={13} />
          </Menu.Trigger>
          <MenuContent align="end">
            {(["AutoCommit", "Manual"] as const).map((m) => (
              <MenuItem
                key={m}
                testId={`tx-mode-${m}`}
                checked={m === tab.console.mode}
                disabled={sqlServer && m === "Manual"}
                onSelect={() => void setTabMode(tab.id, m)}
              >
                {MODE_LABEL[m]}
                {sqlServer && m === "Manual" ? " (unavailable for SQL Server)" : ""}
              </MenuItem>
            ))}
            {sqlServer && <MenuLabel>Use BEGIN, COMMIT and ROLLBACK in SQL scripts.</MenuLabel>}
          </MenuContent>
        </Menu.Root>
      )}

      {tab && hasUncommittedWork(liveConsole) && (
        <>
          <Button
            data-testid="commit-btn"
            title={sqlServer ? "Commit all transaction nesting levels" : undefined}
            disabled={busy}
            onClick={() => void endTransaction(tab.id, "commit")}
          >
            Commit
          </Button>
          <Button data-testid="rollback-btn" disabled={busy} onClick={() => void endTransaction(tab.id, "rollback")}>
            Rollback
          </Button>
        </>
      )}

      {tab && (
        // Format gives way to the run target in a narrow bar, and ⌘⌥L still formats.
        <Button
          data-testid="format-btn"
          title="Format the selection or the marked statement (⌘⌥L)"
          className="@max-[900px]:px-2.5 @max-[520px]:hidden"
          onClick={() => void formatActive()}
        >
          <ListIndentIncreaseIcon size={14} className="text-muted" />
          <span className="@max-[900px]:sr-only">Format</span>
        </Button>
      )}

      {tab && liveConsole?.work === "Count" && (
        <span data-testid="counting-status" role="status" className="text-ui-13 text-dim">
          Counting…
        </span>
      )}

      {tab && <span aria-hidden className="mx-1 h-5 w-px shrink-0 bg-border" />}
      {tab &&
        (cancellable ? (
          <Button
            data-testid="cancel-btn"
            title={
              sqlServer
                ? liveConsole?.work === "Import"
                  ? "Cancel waits for the active import batch, then rolls back."
                  : "Cancel resets the SQL Server session. Earlier commits remain committed."
                : undefined
            }
            className="w-[104px] justify-center bg-hover"
            onClick={() => cancelActive()}
          >
            <SquareIcon fill="currentColor" size={10} className="text-muted" />
            Cancel
            <Kbd tone="menu">Esc</Kbd>
          </Button>
        ) : (
          <RunButton busy={busy} environment={environment} explainable={!!config && explainSupported(config.databaseKind) && !isBrowsing(tab)} />
        ))}

      {layout.rightPanel === null && (
        <>
          <div className="mx-1 h-5 w-px bg-border" />
          <WindowEdgeActions hosted={hosted} />
        </>
      )}
    </header>
  );
}
