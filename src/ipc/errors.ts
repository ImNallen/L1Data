import type {
  ApplyError,
  ColumnTypeIssue,
  EditRefusal,
  ReadOnlyReason,
  SessionRefusal,
  TableKind,
  ApproveError,
  CatalogError,
  ChatError,
  ConfigureError,
  ConnectError,
  DetailError,
  DisconnectError,
  ExportError,
  ImportError,
  FileReadError,
  FileWriteError,
  HistoryClearError,
  RunError,
  SavedQueryError,
  SaveTarget,
  SendError,
  SettingsError,
  SshError,
  TxnError,
  UpdateError,
  UpdateFailure,
  WorkspaceError,
} from "./bindings";

export function connectErrorText(e: ConnectError): string {
  switch (e.kind) {
    case "Network":
    case "Tls":
    case "Auth":
    case "Keychain":
    case "Server":
      return e.message;
    case "MissingPassword":
      return "No password is saved for this connection. Edit the connection to set one.";
    case "Ssh":
      return sshErrorText(e.error);
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

function knownHostsName(host: string, port: number): string {
  return port === 22 ? host : `[${host}]:${port}`;
}

function sshErrorText(e: SshError): string {
  switch (e.kind) {
    case "Network":
    case "KnownHosts":
    case "Auth":
    case "Key":
    case "Agent":
    case "Forward":
      return `SSH: ${e.message}`;
    case "HostKeyUnknown":
      return `SSH: ${e.host}:${e.port} offered an unknown ${e.algorithm} host key ${e.fingerprint}.`;
    case "HostKeyChanged": {
      const file = e.knownAt.slice(0, e.knownAt.lastIndexOf(":"));
      return `SSH: the ${e.algorithm} host key of ${e.host}:${e.port} is now ${e.fingerprint}, but ${e.knownAt} holds another. If the server's key was replaced, run ssh-keygen -R '${knownHostsName(e.host, e.port)}' -f '${file}'.`;
    }
    case "HostKeyRevoked":
      return `SSH: the ${e.algorithm} host key ${e.fingerprint} of ${e.host}:${e.port} is revoked at ${e.revokedAt}.`;
    case "MissingSecret":
      return "SSH: no password or key passphrase is saved for the tunnel. Edit the connection to set one.";
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function runErrorText(e: RunError): string {
  switch (e.kind) {
    case "Busy":
      return "This tab is still running. Cancel it first.";
    case "NoStatement":
      return "No statement at the cursor.";
    case "NotConfigured":
      return "Choose a connection for this tab.";
    case "Unsupported":
      return `Cannot run this SQL. ${e.what}`;
    case "Connect":
      return connectErrorText(e.error);
    case "Reference":
      return e.message;
    case "BadFilter":
      return "WHERE and ORDER BY each take one expression, without a semicolon.";
    case "ConfirmWrites":
      return `Nothing ran. This run writes to Production on ${e.confirmation.connection} / ${e.confirmation.database} and needs your confirmation.`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function applyErrorText(e: ApplyError): string {
  switch (e.kind) {
    case "Unsupported":
      return e.what;
    case "Busy":
      return "This tab is still running. Cancel it first.";
    case "NotConfigured":
      return "Choose a connection for this tab.";
    case "Connect":
      return connectErrorText(e.error);
    case "TransactionAborted":
      return "This tab's transaction failed. Roll it back, then submit again.";
    case "NoPrimaryKey":
      return "Editing rows needs the table's current primary key. Reload the table before retrying.";
    case "Failed":
    case "Transaction":
      return `Could not apply edits. ${e.error.message}`;
    case "OutcomeUnknown":
      return `The outcome of these edits is unknown. Verify the saved data before retrying. ${e.error.message}`;
    case "RowMismatch": {
      const verb = e.change.kind === "Delete" ? "delete" : "update";
      return e.rows === 0
        ? `Could not apply edits. A row to ${verb} no longer exists. Reload and try again.`
        : `Could not apply edits. A row to ${verb} matched ${e.rows} rows.`;
    }
    case "ConfirmWrites":
      return `Nothing was applied. These edits write to Production on ${e.confirmation.connection} / ${e.confirmation.database} and need your confirmation.`;
    case "Refused":
      return `Nothing was applied. Edits are unavailable here: ${editRefusalText(e.reason)}.`;
    case "RowChanged":
      return `Nothing was applied. A row to ${e.change.kind === "Delete" ? "delete" : "update"} changed since it was read. Reload and try again.`;
    case "Unverifiable":
      return `Nothing was applied. The stored value of ${e.column} can't be compared exactly in its code page. Add a rowversion column, or change this value with SQL.`;
    case "TableChanged":
      return "Nothing was applied. The table changed since it was read. Reload and try again.";
    case "Locked":
      return "Nothing was applied. A row or table is locked by another session. Try again when it finishes.";
    case "Session":
      return `Nothing was applied. ${sessionRefusalText(e.reason)}`;
    case "Cancelled":
      return "Cancelled. Nothing was applied.";
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

const TABLE_KINDS: Record<TableKind, string> = {
  MemoryOptimized: "a memory-optimized table",
  Temporal: "a temporal table",
  Graph: "a graph table",
  Ledger: "a ledger table",
  FileTable: "a FileTable",
  External: "an external table",
  Replicated: "a replicated table",
  Stretch: "a Stretch-enabled table",
};

function columnIssueText(issue: ColumnTypeIssue): string {
  switch (issue.kind) {
    case "Clr":
      return "has a CLR type, whose methods run code";
    case "Alias":
      return "has an alias type";
    case "Encrypted":
      return "is Always Encrypted";
    case "BoundRule":
      return "has a bound rule";
    case "BoundDefault":
      return "has a bound default";
    case "Unsupported":
      return `has type ${issue.typeName}, which the grid cannot write`;
    default: {
      const unreachable: never = issue;
      return unreachable;
    }
  }
}

/** Why SQL Server server code could run during a staged edit, as a clause: "dbo.t has an enabled trigger, …". */
export function editRefusalText(reason: EditRefusal): string {
  switch (reason.kind) {
    case "NeedsSqlServer2022":
      return "checking this table needs SQL Server 2022 or later";
    case "Permission":
      return `checking this table needs ${reason.missing} permission`;
    case "HiddenMetadata":
      return "a DENY VIEW DEFINITION or DENY CONTROL permission could hide a trigger or policy from the check";
    case "NoPrimaryKey":
      return "the table has no primary key";
    case "MaskedKey":
      return `the primary key column ${reason.column} is masked`;
    case "TableKind":
      return `${reason.table} is ${TABLE_KINDS[reason.tableKind]}`;
    case "Trigger":
      return `${reason.table} has an enabled trigger, ${reason.trigger}, which could commit or roll back them`;
    case "SecurityPolicy":
      return `${reason.table} has row-level security policy ${reason.policy}`;
    case "IndexedView":
      return `${reason.table} feeds indexed view ${reason.view}`;
    case "IgnoreDupKey":
      return `${reason.table} has index ${reason.index} with IGNORE_DUP_KEY, which drops rows without an error`;
    case "Function":
      return `${reason.table} calls ${reason.object} from a constraint, default, or computed column`;
    case "ColumnType":
      return `${reason.table}.${reason.column} ${columnIssueText(reason.issue)}`;
    case "CascadeTooWide":
      return "its foreign key actions reach more than 64 tables";
    case "Cascade":
      return `${reason.foreignKey} changes rows in ${reason.table}, and ${editRefusalText(reason.cause)}`;
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}

/** A read-only column's header tooltip line. */
export function readOnlyText(reason: ReadOnlyReason): string {
  switch (reason.kind) {
    case "Identity":
      return "Read-only: identity";
    case "Computed":
      return "Read-only: computed";
    case "Rowversion":
      return "Read-only: rowversion";
    case "GeneratedAlways":
      return "Read-only: generated always";
    case "ColumnSet":
      return "Read-only: sparse column set";
    case "Masked":
      return "Read-only: masked";
    case "SqlVariant":
      return "Read-only: sql_variant, whose grid text drops its type";
    case "LegacyLob":
      return "Read-only: text, ntext, or image";
    case "Cascade":
      return `Read-only: changing it cascades through ${reason.foreignKey} to ${reason.table}, and ${editRefusalText(reason.cause)}`;
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}

function sessionRefusalText(reason: SessionRefusal): string {
  switch (reason) {
    case "OpenTransaction":
      return "Commit or roll back this tab's transaction before submitting edits.";
    case "ImplicitTransactions":
      return "This tab has IMPLICIT_TRANSACTIONS on. Turn it off before submitting edits.";
    case "LimitedTextSize":
      return "This tab has a limited SET TEXTSIZE, so its pages may hold cut values. Run SET TEXTSIZE -1, then reload.";
    case "ResultShape":
      return "A setting on this tab, such as SHOWPLAN, NOEXEC, FMTONLY, PARSEONLY, or STATISTICS XML or PROFILE, changes what batches return. Turn it off before submitting edits.";
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}

export function txnErrorText(e: TxnError): string {
  switch (e.kind) {
    case "Busy":
      return "Wait for the run to finish, or cancel it.";
    case "NoTransaction":
      return "There is no open transaction.";
    case "RolledBack":
      return "The transaction had failed, so it was rolled back instead.";
    case "Query":
      return e.message;
    case "ConfirmWrites":
      return `Nothing was committed. This commit keeps writes on Production on ${e.confirmation.connection} / ${e.confirmation.database} and needs your confirmation.`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function configureErrorText(e: ConfigureError): string {
  switch (e.kind) {
    case "Unsupported":
      return e.what;
    case "TransactionOpen":
      return "Commit or roll back the open transaction first.";
    case "Busy":
      return "Wait for the run to finish, or cancel it.";
    case "UnknownConnection":
      return "That connection no longer exists.";
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function disconnectErrorText(e: DisconnectError): string {
  switch (e.kind) {
    case "OpenTransactions":
      return `${e.tabs.length} ${e.tabs.length === 1 ? "tab has" : "tabs have"} an open transaction.`;
    case "Store":
      return e.message;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function exportErrorText(e: ExportError, path: string): string {
  const unchanged = `${path} was not changed.`;
  switch (e.kind) {
    case "Gone":
      return "This result is gone. Run the query again to export it.";
    case "Io":
      return `Couldn't write ${path}: ${e.message}. ${unchanged}`;
    case "NotRerunnable":
      return "Only a read statement runs again to export every row.";
    case "Busy":
      return "This tab is busy. Export when it finishes.";
    case "TargetChanged":
      return "This tab now targets another database. Run the query again, then export.";
    case "TransactionAborted":
      return "The transaction failed. Roll back, then export.";
    case "Connect":
      return connectErrorText(e.error);
    case "Query":
      return `The export query failed: ${e.error.message}. ${unchanged}`;
    case "ShapeChanged":
      return `The query returned other columns when it ran again. ${unchanged}`;
    case "Cancelled":
      return `Export cancelled. ${unchanged}`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

const megabytes = (bytes: number) => `${Number((bytes / (1024 * 1024)).toFixed(1))} MB`;

export function fileReadErrorText(e: FileReadError, path: string): string {
  switch (e.kind) {
    case "NotFound":
      return `${path} no longer exists.`;
    case "NotAFile":
      return `${path} is a folder or a device, not a file.`;
    case "PermissionDenied":
      return `You don't have permission to read ${path}.`;
    case "NotUtf8":
      return `${path} isn't UTF-8 text (byte ${e.offset.toLocaleString("en-US")}). L1 Data opens UTF-8 files only.`;
    case "TooLarge":
      return `${path} is ${megabytes(e.bytes)}. L1 Data opens files up to ${megabytes(e.limit)}.`;
    case "Io":
      return `Couldn't open ${path}: ${e.message}`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

/** A save may fail for want of permission to read the file, to check it for outside changes, or to write its folder. */
export function fileWriteErrorText(e: FileWriteError, target: SaveTarget): string {
  const bound = target.kind === "Bound";
  const path = bound ? target.binding.path : target.path;
  switch (e.kind) {
    case "Changed":
      return `${path} changed on disk.`;
    case "NotFound":
      return bound ? `The folder for ${path} no longer exists. Use Save As to pick another place.` : `The folder for ${path} doesn't exist.`;
    case "NotAFile":
      return `${path} is a folder or a device, not a file.`;
    case "PermissionDenied":
      return bound ? `You don't have permission to save ${path}. Use Save As to save a copy elsewhere.` : `You don't have permission to save ${path}.`;
    case "OpenInAnotherNotebook":
      return `${e.path} is open in another notebook. Delete that notebook before you save over its file.`;
    case "Io":
      return `Couldn't save ${path}: ${e.message}`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function savedQueryErrorText(e: SavedQueryError): string {
  switch (e.kind) {
    case "NothingToSave":
      return "There is no SQL to save. Select some, or put the cursor in a statement.";
    case "TooLong":
      return `A saved query holds up to ${e.limitBytes / 1024} KB of SQL. Save a longer script as a .sql file.`;
    case "NotFound":
      return "That saved query was deleted.";
    case "Store":
      return `Couldn't save the query: ${e.message}`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function settingsErrorText(e: SettingsError): string {
  switch (e.kind) {
    case "Unsaved":
      return `Couldn't save settings: ${e.message}. They apply until you quit.`;
    default: {
      const unreachable: never = e.kind;
      return unreachable;
    }
  }
}

export function workspaceErrorText(e: WorkspaceError): string {
  switch (e.kind) {
    case "Store":
      return `Couldn't save the workspace: ${e.message}. Changes since the last save won't reopen after you quit.`;
    default: {
      const unreachable: never = e.kind;
      return unreachable;
    }
  }
}

export function historyClearErrorText(e: HistoryClearError): string {
  switch (e.kind) {
    case "Store":
      return `Couldn't clear history: ${e.message}`;
    default: {
      const unreachable: never = e.kind;
      return unreachable;
    }
  }
}

export function catalogErrorText(e: CatalogError): string {
  switch (e.kind) {
    case "NotConnected":
      return "The connection is closed.";
    case "UnknownDatabase":
      return `The database ${e.database} no longer exists.`;
    case "NotShown":
      return `The database ${e.database} is not shown.`;
    case "Failed":
      return e.message;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function detailErrorText(e: DetailError): string {
  switch (e.kind) {
    case "Catalog":
      return catalogErrorText(e.error);
    case "Gone":
      return "This object no longer exists.";
    case "Failed":
      return e.message;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function sendErrorText(e: SendError): string {
  switch (e.kind) {
    case "Busy":
      return "The assistant is still answering. Stop it or wait for it to finish.";
    case "EmptyMessage":
      return "Type a message first.";
    case "ProviderOff":
      return `${e.provider} is turned off. Turn it on in Settings → Providers.`;
    case "UnknownModel":
      return `The model ${e.model} is no longer offered. Pick another model.`;
    case "AttachmentGone":
      return "The attached result is gone. Run the query again, then attach it.";
    case "Store":
      return `The chat could not be saved. ${e.message}`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function chatErrorText(e: ChatError): string {
  switch (e.kind) {
    case "UnknownModel":
      return `The model ${e.model} is no longer offered. Pick another model.`;
    case "UnsupportedEffort":
      return `This model has no ${e.effort} effort level.`;
    case "Store":
      return `The chat could not be saved. ${e.message}`;
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function approveErrorText(e: ApproveError): string {
  switch (e.kind) {
    case "NotPending":
      return "This query was already answered, stopped, or expired.";
    default: {
      const unreachable: never = e.kind;
      return unreachable;
    }
  }
}

export function updateErrorText(e: UpdateError): string | null {
  switch (e.kind) {
    case "Stale":
      return null;
    case "Busy":
      return "Wait for the update to finish downloading or installing before you switch tracks.";
    default: {
      const unreachable: never = e;
      return unreachable;
    }
  }
}

export function updateFailureText(f: UpdateFailure): string {
  switch (f.kind) {
    case "Offline":
      return "Couldn't reach GitHub for updates.";
    case "BadSignature":
      return "The update's signature didn't match, so it was discarded.";
    case "ReadOnlyLocation":
      return "L1 Data can't replace itself where it runs. On macOS, move it to Applications.";
    case "PermissionDenied":
      return "L1 Data couldn't replace itself. Check that you can change the folder it runs from.";
    case "Other":
      return `The update failed: ${f.message}`;
    default: {
      const unreachable: never = f;
      return unreachable;
    }
  }
}

export function importErrorText(error: ImportError): string {
  switch (error.kind) {
    case "NotConfigured":
      return "Choose a connection for the table tab.";
    case "Unsupported":
      return "CSV import is available for supported PostgreSQL and SQL Server tables.";
    case "Busy":
      return "The table tab is busy. Wait for its current work to finish.";
    case "Gone":
      return "The prepared CSV was discarded. Choose the file again.";
    case "TargetChanged":
      return "The table or connection changed. Choose the file again.";
    case "TransactionAborted":
      return "Rollback the failed transaction before importing.";
    case "Cancelled":
      return "Import cancelled. No import rows were kept.";
    case "Io":
    case "Mapping":
      return error.message;
    case "Csv":
      return `CSV record ${error.record}, line ${error.line}: ${error.message}`;
    case "Connect":
      return connectErrorText(error.error);
    case "Query":
      return [error.error.message, error.error.context, "The import was rolled back."].filter(Boolean).join("\n");
    case "OutcomeUnknown":
      return `${error.error.message} The outcome is unknown. Check the database before trying again.`;
    case "ConfirmWrites":
      return "Production confirmation is required.";
    default: {
      const unreachable: never = error;
      return unreachable;
    }
  }
}
