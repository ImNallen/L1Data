import type {
  ConnectionConfig,
  ConnectionDraft,
  DatabaseKind,
  Environment,
  PasswordInput,
  SqlServerAuth,
  SshAuth,
  SshTunnelDraft,
  SslMode,
} from "../../ipc/bindings";
import { connectionColorId, type ConnectionColorId } from "./connectionColors";

export type SshAuthKind = SshAuth["kind"];

export type Fields = {
  databaseKind: DatabaseKind;
  environment: Environment | null;
  name: string;
  host: string;
  port: string;
  database: string;
  user: string;
  sqlServerAuth: SqlServerAuth;
  sslMode: SslMode;
  caCertPath: string;
  color: ConnectionColorId | null;
  sshEnabled: boolean;
  sshHost: string;
  sshPort: string;
  sshUser: string;
  sshAuth: SshAuthKind;
  sshKeyPath: string;
  sshAgentSocket: string;
};

export type PasswordEdit = { kind: "keep" } | { kind: "set"; value: string } | { kind: "clear" };

export type FormState = { fields: Fields; password: PasswordEdit; sshSecret: PasswordEdit; url: string };

export function initialFields(c: ConnectionConfig | undefined, databaseKind: DatabaseKind = c?.databaseKind ?? "Postgres"): Fields {
  const sqlServer = databaseKind === "SqlServer";
  const ssh = c?.ssh;
  return {
    databaseKind,
    environment: c?.environment ?? null,
    name: c?.name ?? "",
    host: c?.host ?? "localhost",
    port: String(c?.port ?? (sqlServer ? 1433 : 5432)),
    database: c?.database ?? (sqlServer ? "master" : "postgres"),
    user: c?.user ?? (sqlServer ? "sa" : "postgres"),
    sqlServerAuth: c?.sqlServerAuth ?? "SqlLogin",
    sslMode: c?.sslMode ?? (sqlServer ? "VerifyFull" : "Prefer"),
    caCertPath: c?.caCertPath ?? "",
    color: connectionColorId(c?.color ?? null),
    sshEnabled: ssh != null,
    sshHost: ssh?.host ?? "",
    sshPort: String(ssh?.port ?? 22),
    sshUser: ssh?.user ?? "",
    sshAuth: ssh?.auth.kind ?? "Password",
    sshKeyPath: ssh?.auth.kind === "Key" ? ssh.auth.path : "",
    sshAgentSocket: ssh?.auth.kind === "Agent" ? (ssh.auth.socket ?? "") : "",
  };
}

export function initialPassword(c: ConnectionConfig | undefined): PasswordEdit {
  return c?.hasPassword ? { kind: "keep" } : { kind: "clear" };
}

/** The engine keeps a saved SSH secret only under the auth kind it was saved for, so any other kind starts empty. */
export function initialSshSecret(c: ConnectionConfig | undefined, auth: SshAuthKind | undefined = c?.ssh?.auth.kind): PasswordEdit {
  return c?.ssh?.hasSecret && c.ssh.auth.kind === auth ? { kind: "keep" } : { kind: "clear" };
}

/** Whether the form differs from what `existing`, or a new form of its kind, would open it with. */
export function isDirty(existing: ConnectionConfig | undefined, form: FormState): boolean {
  const start = initialFields(existing, form.fields.databaseKind);
  const fieldChanged = (Object.keys(start) as (keyof Fields)[]).some((k) => start[k] !== form.fields[k]);
  // The initial edits are never `set`, so comparing kinds is enough.
  return fieldChanged || form.password.kind !== initialPassword(existing).kind || form.sshSecret.kind !== initialSshSecret(existing).kind || form.url !== "";
}

/** The fields a parsed URL sets. The kind stays the form's own. */
export function withUrl(fields: Fields, d: ConnectionDraft): Fields {
  return {
    ...fields,
    caCertPath: d.caCertPath ?? "",
    host: d.host,
    port: String(d.port),
    database: d.database,
    user: d.user,
    sqlServerAuth: d.sqlServerAuth ?? "SqlLogin",
    sslMode: d.sslMode,
  };
}

/** Only a new connection takes a URL, so a URL without a password leaves none. */
export function urlPassword(d: ConnectionDraft): PasswordEdit {
  return typeof d.password === "object" ? { kind: "set", value: d.password.Set } : { kind: "clear" };
}

export function toSqlServerAuth(fields: Fields): SqlServerAuth | null {
  return fields.databaseKind === "SqlServer" ? fields.sqlServerAuth : null;
}

export function signsInWithEntra(fields: Fields): boolean {
  return toSqlServerAuth(fields) === "EntraAzureCli";
}

export function takesPassword(fields: Fields): boolean {
  return fields.databaseKind === "Postgres" || fields.sqlServerAuth === "SqlLogin";
}

export function toUser(fields: Fields): string {
  return takesPassword(fields) ? fields.user.trim() : "";
}

export function toPassword(fields: Fields, edit: PasswordEdit): PasswordInput {
  return takesPassword(fields) ? toPasswordInput(edit) : "Clear";
}

export function entraNeedsVerifiedTls(fields: Fields): boolean {
  return signsInWithEntra(fields) && fields.sslMode === "Require";
}

export function parsePort(text: string): number | null {
  const port = Number(text);
  return Number.isInteger(port) && port > 0 && port < 65_536 ? port : null;
}

export function toPasswordInput(edit: PasswordEdit): PasswordInput {
  switch (edit.kind) {
    case "keep":
      return "Keep";
    case "clear":
      return "Clear";
    case "set":
      return { Set: edit.value };
    default: {
      const unreachable: never = edit;
      return unreachable;
    }
  }
}

/** The tunnel the form describes, or `null` when it is off. Agent auth sends `Clear`, since it takes no secret. */
export function toSshDraft(fields: Fields, sshSecret: PasswordEdit): SshTunnelDraft | null {
  if (!fields.sshEnabled) return null;
  const tunnel = { host: fields.sshHost.trim(), port: parsePort(fields.sshPort) ?? 22, user: fields.sshUser.trim() };
  switch (fields.sshAuth) {
    case "Password":
      return { ...tunnel, auth: { kind: "Password" }, secret: toPasswordInput(sshSecret) };
    case "Key":
      return { ...tunnel, auth: { kind: "Key", path: fields.sshKeyPath.trim() }, secret: toPasswordInput(sshSecret) };
    case "Agent":
      return { ...tunnel, auth: { kind: "Agent", socket: fields.sshAgentSocket.trim() || null }, secret: "Clear" };
    default: {
      const unreachable: never = fields.sshAuth;
      return unreachable;
    }
  }
}

/** Whether the tunnel fields are complete enough to dial. An off tunnel always is. */
export function sshValid(fields: Fields): boolean {
  if (!fields.sshEnabled) return true;
  return (
    fields.sshHost.trim() !== "" &&
    parsePort(fields.sshPort) !== null &&
    fields.sshUser.trim() !== "" &&
    (fields.sshAuth !== "Key" || fields.sshKeyPath.trim() !== "")
  );
}

export function unreadableConnectionsNotice(count: number): string | null {
  if (count === 0) return null;
  return count === 1
    ? "1 saved connection could not be read, so it is hidden. A newer version of L1 Data may have saved it."
    : `${count} saved connections could not be read, so they are hidden. A newer version of L1 Data may have saved them.`;
}
