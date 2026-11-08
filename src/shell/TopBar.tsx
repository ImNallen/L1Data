import * as Menu from "@radix-ui/react-dropdown-menu";
import { BookOpenIcon, ChevronDownIcon, DatabaseIcon, ListIndentIncreaseIcon, PlayIcon, PlusIcon, SquareIcon } from "lucide-react";
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
  setTabSchema,
  openSqlTab,
} from "../features/editor/runActions";
import type { ConnectionState, DatabaseRef, Environment, TransactionLoss, TxnMode } from "../ipc/bindings";
import { useCatalog, viewFor } from "../state/catalog";
import { homeDb } from "../state/dbKey";
import { instanceOf, useLive } from "../state/live";
import { canCancel, connectionTone, hasUncommittedWork, isBusy } from "../state/status";
import { notebookFor } from "../state/notebooks";
import { openNotebookSwitcher } from "../features/notebooks/notebookActions";
import { activeTab, shownDb, shownNotebook, useWorkspace } from "../state/workspace";
import { Button, CONNECTION_TONE_LABEL, Kbd, MenuContent, MenuItem, MenuLabel, StatusDot, IconButton, cx } from "../ui/primitives";
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

function DatabaseMenu({ db }: { db: DatabaseRef }) {
  const state = useLive((s) => s.connections[db.connectionId]);
  const showDatabase = useWorkspace((s) => s.showDatabase);
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger data-testid="tab-database" title={db.database} className={cx(TARGET_TRIGGER, "flex-1 text-fg")}>
        <span className="truncate">{db.database}</span>
        <ChevronDownIcon size={12} className="shrink-0 text-dim" />
      </Menu.Trigger>
      <MenuContent>
        <MenuLabel>Switch to database</MenuLabel>
        {databaseChoices(db, state).map((name) => (
          <MenuItem
            key={name}
            testId={`tab-database-${name}`}
            checked={name === db.database}
            onSelect={() => showDatabase({ connectionId: db.connectionId, database: name })}
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

export function TopBar({ hosted, notebook = false }: { hosted: HostedWindow | null; notebook?: boolean }) {
  const tab = useWorkspace(activeTab);
  const db = useWorkspace(shownDb);
  const unconnected = useWorkspace((s) => notebookFor(s.notebooks, null) !== undefined);
  const notebookTitle = useWorkspace((s) => shownNotebook(s)?.title);
  const switching = useWorkspace((s) => s.overlay.kind === "notebooks");
  const showDatabase = useWorkspace((s) => s.showDatabase);
  const configs = useWorkspace((s) => s.configs);
  const layout = useWorkspace((s) => s.layout);
  const connectionId = db?.connectionId ?? null;
  const liveConsole = useLive((s) => (tab ? s.consoles[tab.id] : undefined));
  const connections = useLive((s) => s.connections);
  const catalog = useCatalog((s) => viewFor(s.views, db)?.catalog);
  const config = connectionId ? configs[connectionId] : undefined;
  const busy = isBusy(liveConsole);
  const cancellable = canCancel(liveConsole);
  const txn = liveConsole?.txn;
  const txnDepth = liveConsole?.txnDepth;
  const sqlServer = config?.databaseKind === "SqlServer";
  const loss = liveConsole?.session.kind === "Lost" ? LOSS_LABEL[liveConsole.session.transaction] : null;
  const environment = config?.environment ?? null;
  const pending = hasUncommittedWork(liveConsole);
  const expandedActions = pending || loss !== null || liveConsole?.work === "Count";

  return (
    <header
      data-tauri-drag-region="deep"
      className={cx(
        notebook ? "workspace-context @container shrink-0" : "@container shrink-0 border-b-2 border-transparent pt-[2px]",
        windowEdgeInset(hosted),
        layout.sidebar ? "pl-4" : windowLeadingInset(hosted),
      )}
      data-testid="top-bar"
      data-environment={environment ?? undefined}
    >
      <div
        className={cx(
          "flex min-h-12 min-w-0 items-center gap-2",
          expandedActions ? "@max-[760px]:flex-wrap @max-[760px]:gap-y-1 @max-[760px]:py-1" : "@max-[520px]:flex-wrap @max-[520px]:gap-y-1 @max-[520px]:py-1",
        )}
      >
        <div className={cx("flex min-w-0 flex-1 items-center gap-2", expandedActions ? "@max-[760px]:basis-full" : "@max-[520px]:basis-full")}>
          {!layout.sidebar && <SidebarToggle />}
          {tab || db ? (
            <Menu.Root modal={false}>
              <Menu.Trigger data-testid="tab-connection" title={config?.name} className={cx(TARGET_TRIGGER, "-ml-1 flex-1", config ? "text-fg" : "text-dim")}>
                {notebook ? (
                  <DatabaseIcon size={14} className="shrink-0 text-keyword" />
                ) : (
                  <ConnectionColorDot color={config?.color ?? null} className="mr-0.5" />
                )}
                <span className="truncate">{config?.name ?? "No connection"}</span>
                <ChevronDownIcon size={12} className="shrink-0 text-dim" />
              </Menu.Trigger>
              <MenuContent>
                <MenuLabel>Switch to connection</MenuLabel>
                {Object.values(configs).map((c) => {
                  const tone = connectionTone(connections[c.id]);
                  return (
                    <MenuItem key={c.id} checked={c.id === connectionId} onSelect={() => showDatabase(homeDb(c, connections[c.id]))}>
                      <StatusDot tone={tone} label={CONNECTION_TONE_LABEL[tone]} />
                      <ConnectionColorDot color={c.color} />
                      {c.name}
                    </MenuItem>
                  );
                })}
                {unconnected && (
                  <MenuItem checked={connectionId === null} onSelect={() => showDatabase(null)}>
                    No connection
                  </MenuItem>
                )}
              </MenuContent>
            </Menu.Root>
          ) : (
            <span className="flex min-w-0 items-center gap-1.5 text-ui-14 text-dim">
              <ConnectionColorDot color={config?.color ?? null} />
              <span className="truncate">{config?.name ?? ""}</span>
            </span>
          )}
          {db && config && (
            <div className={cx("flex min-w-0 items-center overflow-hidden", !notebook && "flex-1")}>
              <Separator />
              <DatabaseMenu db={db} />
            </div>
          )}
          {notebook && tab && db && (
            <>
              <Separator />
              <Menu.Root modal={false}>
                <Menu.Trigger data-testid="tab-schema" className={cx(TARGET_TRIGGER, "shrink-0")}>
                  {tab.console.schema ?? catalog?.defaultSchema ?? "Schema"}
                  <ChevronDownIcon size={12} className="text-dim" />
                </Menu.Trigger>
                <MenuContent>
                  <MenuItem checked={!tab.console.schema} onSelect={() => void setTabSchema(tab.id, null)}>
                    Default schema
                  </MenuItem>
                  {catalog?.schemas.map((schema) => (
                    <MenuItem key={schema.name} checked={tab.console.schema === schema.name} onSelect={() => void setTabSchema(tab.id, schema.name)}>
                      {schema.name}
                    </MenuItem>
                  ))}
                </MenuContent>
              </Menu.Root>
              {environment && (
                <>
                  <Separator />
                  <span
                    data-testid="environment-marker"
                    className={cx("shrink-0 text-ui-10", environment === "Production" ? "text-danger" : environment === "Staging" ? "text-amber" : "text-dim")}
                  >
                    {environment === "Local" ? "Develop" : environment}
                  </span>
                </>
              )}
            </>
          )}
        </div>

        <div
          className={cx(
            "flex min-w-0 shrink-0 items-center gap-2",
            expandedActions ? "@max-[760px]:basis-full @max-[760px]:flex-wrap" : "@max-[520px]:basis-full @max-[520px]:flex-wrap",
          )}
        >
          {tab && <span aria-hidden className={cx("h-5 w-px shrink-0 bg-border", expandedActions ? "@max-[760px]:hidden" : "@max-[520px]:hidden")} />}
          {tab && (
            <Menu.Root modal={false}>
              <Menu.Trigger
                data-testid="tx-mode"
                title={
                  loss?.title ??
                  (txn === "Open" ? "Transaction open. SQL Server does not report nesting depth or committability through the protocol." : undefined)
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

          {tab && pending && (
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

          {tab && !notebook && (
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

          {tab && !notebook && <span aria-hidden className="mx-1 h-5 w-px shrink-0 bg-border" />}
          {tab &&
            !notebook &&
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

          {notebook && (
            <IconButton
              data-testid="notebook-switcher-btn"
              aria-label="Notebooks"
              aria-pressed={switching}
              title={notebookTitle ?? "Notebooks"}
              className={cx(switching && "bg-badge-bg text-badge hover:bg-badge-bg hover:text-badge")}
              onClick={openNotebookSwitcher}
            >
              <BookOpenIcon size={14} />
            </IconButton>
          )}
          {notebook && (
            <IconButton data-testid="new-query-cell" aria-label="Add SQL cell" title="Add SQL cell" onClick={() => openSqlTab()}>
              <PlusIcon size={14} />
            </IconButton>
          )}
          {layout.rightPanel === null && (
            <>
              <div className="mx-1 h-5 w-px bg-border" />
              <WindowEdgeActions hosted={hosted} />
            </>
          )}
        </div>
      </div>
    </header>
  );
}
