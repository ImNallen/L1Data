import * as Menu from "@radix-ui/react-dropdown-menu";
import { BookOpenIcon, ChevronDownIcon, DatabaseIcon, PlusIcon } from "lucide-react";
import { ConnectionColorDot } from "../features/connections/connectionColors";
import { endTransaction, setTabMode, setTabSchema, openSqlTab } from "../features/editor/runActions";
import type { ConnectionState, DatabaseRef, TransactionLoss, TxnMode } from "../ipc/bindings";
import { useCatalog, viewFor } from "../state/catalog";
import { homeDb } from "../state/dbKey";
import { instanceOf, useLive } from "../state/live";
import { connectionTone, hasUncommittedWork, isBusy } from "../state/status";
import { notebookFor } from "../state/notebooks";
import { openNotebookSwitcher } from "../features/notebooks/notebookActions";
import { useNotebookDirty } from "../features/notebooks/notebookDirty";
import { NotebookRunButton } from "../features/notebooks/NotebookRunButton";
import { activeQueryTab, activeTab, shownDb, shownNotebook, useWorkspace } from "../state/workspace";
import { Button, CONNECTION_TONE_LABEL, MenuContent, MenuItem, MenuLabel, StatusDot, IconButton, cx } from "../ui/primitives";
import { windowEdgeClearance, windowEdgeInset, windowLeadingClearance } from "./WindowEdgeActions";
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

export function TopBar({ hosted }: { hosted: HostedWindow | null }) {
  const tab = useWorkspace(activeTab);
  const queryTab = useWorkspace(activeQueryTab);
  const db = useWorkspace(shownDb);
  const unconnected = useWorkspace((s) => notebookFor(s.notebooks, null) !== undefined);
  const notebookId = useWorkspace((s) => shownNotebook(s)?.id);
  const notebookTitle = useWorkspace((s) => shownNotebook(s)?.title);
  const notebookUnsaved = useNotebookDirty(notebookId);
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
        "workspace-context @container shrink-0 follow-panels",
        layout.rightPanel === null ? windowEdgeClearance(hosted) : windowEdgeInset(hosted),
        layout.sidebar ? "pl-[24px] max-[1100px]:pl-[16px]" : windowLeadingClearance(hosted),
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
          {tab || db ? (
            <Menu.Root modal={false}>
              <Menu.Trigger data-testid="tab-connection" title={config?.name} className={cx(TARGET_TRIGGER, "-ml-1 flex-1", config ? "text-fg" : "text-dim")}>
                <DatabaseIcon size={14} className="shrink-0 text-keyword" />
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
            <div className="flex min-w-0 items-center overflow-hidden">
              <Separator />
              <DatabaseMenu db={db} />
            </div>
          )}
          {tab && db && (
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
          {queryTab && <span aria-hidden className={cx("h-5 w-px shrink-0 bg-border", expandedActions ? "@max-[760px]:hidden" : "@max-[520px]:hidden")} />}
          {queryTab && (
            <Menu.Root modal={false}>
              <Menu.Trigger
                data-testid="tx-mode"
                title={
                  loss?.title ??
                  (txn === "Open" ? "Transaction open. SQL Server does not report nesting depth or committability through the protocol." : undefined)
                }
                className="inline-flex h-7 items-center gap-1 rounded-md pr-1.5 pl-2 text-ui-12.5 text-muted hover:bg-hover hover:text-fg data-[state=open]:bg-hover data-[state=open]:text-fg"
              >
                {MODE_LABEL[queryTab.console.mode]}
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
                    checked={m === queryTab.console.mode}
                    disabled={sqlServer && m === "Manual"}
                    onSelect={() => void setTabMode(queryTab.id, m)}
                  >
                    {MODE_LABEL[m]}
                    {sqlServer && m === "Manual" ? " (unavailable for SQL Server)" : ""}
                  </MenuItem>
                ))}
                {sqlServer && <MenuLabel>Use BEGIN, COMMIT and ROLLBACK in SQL scripts.</MenuLabel>}
              </MenuContent>
            </Menu.Root>
          )}

          {queryTab && pending && (
            <>
              <Button
                data-testid="commit-btn"
                title={sqlServer ? "Commit all transaction nesting levels" : undefined}
                disabled={busy}
                onClick={() => void endTransaction(queryTab.id, "commit")}
              >
                Commit
              </Button>
              <Button data-testid="rollback-btn" disabled={busy} onClick={() => void endTransaction(queryTab.id, "rollback")}>
                Rollback
              </Button>
            </>
          )}

          {queryTab && liveConsole?.work === "Count" && (
            <span data-testid="counting-status" role="status" className="text-ui-13 text-dim">
              Counting…
            </span>
          )}

          {notebookId && <NotebookRunButton id={notebookId} environment={environment} />}
          <IconButton
            data-testid="notebook-switcher-btn"
            aria-label={notebookUnsaved ? "Notebooks, unsaved changes" : "Notebooks"}
            aria-pressed={switching}
            title={notebookUnsaved ? `${notebookTitle} has unsaved changes` : (notebookTitle ?? "Notebooks")}
            className={cx("relative", switching && "bg-badge-bg text-badge hover:bg-badge-bg hover:text-badge")}
            onClick={openNotebookSwitcher}
          >
            <BookOpenIcon size={14} />
            {notebookUnsaved && (
              <span data-testid="notebook-unsaved" aria-hidden className="absolute top-1 right-1 size-1.5 rounded-full border-[1.5px] border-focus bg-bg" />
            )}
          </IconButton>
          <IconButton data-testid="new-query-cell" aria-label="Add SQL cell" title="Add SQL cell" onClick={() => openSqlTab()}>
            <PlusIcon size={14} />
          </IconButton>
        </div>
      </div>
    </header>
  );
}
