import { expect, it } from "vitest";
import type { ConnectionConfig } from "../../ipc/bindings";
import {
  entraNeedsVerifiedTls,
  initialFields,
  initialSshSecret,
  isDirty,
  signsInWithEntra,
  sshValid,
  takesPassword,
  toPassword,
  toSqlServerAuth,
  toSshDraft,
  toUser,
  unreadableConnectionsNotice,
  type FormState,
  type PasswordEdit,
} from "./connectionForm";

const saved: ConnectionConfig = {
  id: "c1",
  databaseKind: "Postgres",
  name: "shop",
  host: "db.internal",
  port: 5433,
  database: "shop",
  user: "l1data",
  sslMode: "Require",
  caCertPath: null,
  hasPassword: true,
  color: "red",
  ssh: null,
  environment: null,
  sqlServerAuth: null,
};

const savedFields = {
  databaseKind: "Postgres",
  environment: null,
  caCertPath: "",
  name: "shop",
  host: "db.internal",
  port: "5433",
  database: "shop",
  user: "l1data",
  sqlServerAuth: "SqlLogin",
  sslMode: "Require",
  color: "red",
  sshEnabled: false,
  sshHost: "",
  sshPort: "22",
  sshUser: "",
  sshAuth: "Password",
  sshKeyPath: "",
  sshAgentSocket: "",
} as const;

const newFields = {
  databaseKind: "Postgres",
  environment: null,
  caCertPath: "",
  name: "",
  host: "localhost",
  port: "5432",
  database: "postgres",
  user: "postgres",
  sqlServerAuth: "SqlLogin",
  sslMode: "Prefer",
  color: null,
  sshEnabled: false,
  sshHost: "",
  sshPort: "22",
  sshUser: "",
  sshAuth: "Password",
  sshKeyPath: "",
  sshAgentSocket: "",
} as const;

it("reads an untouched new form as clean", () => {
  expect(isDirty(undefined, { fields: { ...newFields }, password: { kind: "clear" }, sshSecret: { kind: "clear" }, url: "" })).toBe(false);
});

it("reads a changed host as dirty", () => {
  expect(isDirty(undefined, { fields: { ...newFields, host: "db.internal" }, password: { kind: "clear" }, sshSecret: { kind: "clear" }, url: "" })).toBe(true);
});

it("reads a new form with only a URL typed as dirty", () => {
  expect(isDirty(undefined, { fields: { ...newFields }, password: { kind: "clear" }, sshSecret: { kind: "clear" }, url: "postgres://" })).toBe(true);
});

it("reads a kept saved password as clean and a cleared one as dirty", () => {
  expect(isDirty(saved, { fields: { ...savedFields }, password: { kind: "keep" }, sshSecret: { kind: "clear" }, url: "" })).toBe(false);
  expect(isDirty(saved, { fields: { ...savedFields }, password: { kind: "clear" }, sshSecret: { kind: "clear" }, url: "" })).toBe(true);
});

it("compares color against the normalized id", () => {
  expect(isDirty(saved, { fields: { ...savedFields }, password: { kind: "keep" }, sshSecret: { kind: "clear" }, url: "" })).toBe(false);
  expect(
    isDirty({ ...saved, color: "#ef4444" }, { fields: { ...savedFields, color: null }, password: { kind: "keep" }, sshSecret: { kind: "clear" }, url: "" }),
  ).toBe(false);
});

it("opens a saved environment and reads a change to it as an edit", () => {
  const production: ConnectionConfig = { ...saved, environment: "Production" };
  expect(initialFields(production).environment).toBe("Production");
  expect(
    isDirty(production, { fields: { ...savedFields, environment: "Production" }, password: { kind: "keep" }, sshSecret: { kind: "clear" }, url: "" }),
  ).toBe(false);
  expect(isDirty(production, { fields: { ...savedFields, environment: null }, password: { kind: "keep" }, sshSecret: { kind: "clear" }, url: "" })).toBe(true);
});

it("starts SQL Server with SQL login, master and verified TLS", () => {
  expect(initialFields(undefined, "SqlServer")).toMatchObject({
    databaseKind: "SqlServer",
    port: "1433",
    database: "master",
    user: "sa",
    sslMode: "VerifyFull",
    caCertPath: "",
  });
  const fields = initialFields({ ...saved, databaseKind: "SqlServer", port: 1434, database: "reporting", sslMode: "Require" });
  expect(fields).toMatchObject({ databaseKind: "SqlServer", port: "1434", database: "reporting", sslMode: "Require" });
  expect(isDirty(undefined, { fields: initialFields(undefined, "SqlServer"), password: { kind: "clear" }, sshSecret: { kind: "clear" }, url: "" })).toBe(false);
  expect(
    isDirty(undefined, {
      fields: { ...initialFields(undefined, "SqlServer"), port: "1434" },
      password: { kind: "clear" },
      sshSecret: { kind: "clear" },
      url: "",
    }),
  ).toBe(true);
});

it("opens a connection saved before Entra as a SQL login, and a saved Entra one as Entra", () => {
  const sqlServer: ConnectionConfig = { ...saved, databaseKind: "SqlServer", sqlServerAuth: null };
  expect(initialFields(sqlServer).sqlServerAuth).toBe("SqlLogin");
  expect(isDirty(sqlServer, { fields: initialFields(sqlServer), password: { kind: "keep" }, sshSecret: { kind: "clear" }, url: "" })).toBe(false);
  const azure: ConnectionConfig = { ...sqlServer, sqlServerAuth: "EntraAzureCli", hasPassword: false, user: "" };
  expect(signsInWithEntra(initialFields(azure))).toBe(true);
  expect(
    isDirty(azure, { fields: { ...initialFields(azure), sqlServerAuth: "SqlLogin" }, password: { kind: "clear" }, sshSecret: { kind: "clear" }, url: "" }),
  ).toBe(true);
});

it("sends a sign-in choice only for SQL Server", () => {
  const entra = { ...initialFields(undefined, "SqlServer"), sqlServerAuth: "EntraAzureCli" } as const;
  expect(toSqlServerAuth(entra)).toBe("EntraAzureCli");
  expect(toSqlServerAuth(initialFields(undefined, "SqlServer"))).toBe("SqlLogin");
  expect(toSqlServerAuth({ ...entra, databaseKind: "Postgres" })).toBeNull();
  expect(signsInWithEntra({ ...entra, databaseKind: "Postgres" })).toBe(false);
});

it("sends no user name for Entra, whose user field is hidden", () => {
  const sqlServer = initialFields(undefined, "SqlServer");
  expect(toUser({ ...sqlServer, user: " sa " })).toBe("sa");
  expect(toUser({ ...sqlServer, sqlServerAuth: "EntraAzureCli" })).toBe("");
});

it("sends no password for Entra, even one typed under SQL login before the switch", () => {
  const sqlServer = initialFields(undefined, "SqlServer");
  const entra = { ...sqlServer, sqlServerAuth: "EntraAzureCli" } as const;
  const typed: PasswordEdit = { kind: "set", value: "hunter2" };
  expect(toPassword(sqlServer, typed)).toEqual({ Set: "hunter2" });
  expect(toPassword(entra, typed)).toBe("Clear");
  expect(toPassword(entra, { kind: "keep" })).toBe("Clear");
  expect(toPassword({ ...entra, databaseKind: "Postgres" }, typed)).toEqual({ Set: "hunter2" });
});

it("asks for a user and password only for PostgreSQL and a SQL login", () => {
  const sqlServer = initialFields(undefined, "SqlServer");
  expect(takesPassword(initialFields(undefined))).toBe(true);
  expect(takesPassword(sqlServer)).toBe(true);
  expect(takesPassword({ ...sqlServer, sqlServerAuth: "EntraAzureCli" })).toBe(false);
  expect(takesPassword({ ...sqlServer, sqlServerAuth: "Unknown" })).toBe(false);
  expect(toUser({ ...sqlServer, sqlServerAuth: "Unknown" })).toBe("");
});

it("refuses Entra over TLS that skips the certificate check", () => {
  const entra = { ...initialFields(undefined, "SqlServer"), sqlServerAuth: "EntraAzureCli" } as const;
  expect(entraNeedsVerifiedTls(entra)).toBe(false);
  expect(entraNeedsVerifiedTls({ ...entra, sslMode: "Require" })).toBe(true);
  expect(entraNeedsVerifiedTls({ ...entra, sslMode: "Require", sqlServerAuth: "SqlLogin" })).toBe(false);
});

const tunneled: ConnectionConfig = {
  ...saved,
  ssh: { host: "bastion", port: 2222, user: "tunnel", auth: { kind: "Key", path: "~/.ssh/id_ed25519" }, hasSecret: true },
};
const keep: PasswordEdit = { kind: "keep" };
const clear: PasswordEdit = { kind: "clear" };

function form(over: Partial<FormState["fields"]>, sshSecret: PasswordEdit = keep): FormState {
  return { fields: { ...initialFields(tunneled), ...over }, password: keep, sshSecret, url: "" };
}

it("opens a saved tunnel clean and saves it unchanged with its passphrase kept", () => {
  const start = form({});
  expect(isDirty(tunneled, start)).toBe(false);
  expect(toSshDraft(start.fields, start.sshSecret)).toEqual({
    host: "bastion",
    port: 2222,
    user: "tunnel",
    auth: { kind: "Key", path: "~/.ssh/id_ed25519" },
    secret: "Keep",
  });
});

it("sends no tunnel when it is off, and turning a saved one off is an edit", () => {
  expect(toSshDraft(initialFields(saved), clear)).toBeNull();
  expect(isDirty(tunneled, form({ sshEnabled: false }))).toBe(true);
});

it("keeps a saved secret only under the auth kind it was saved for", () => {
  expect(initialSshSecret(tunneled, "Key")).toEqual(keep);
  expect(initialSshSecret(tunneled, "Password")).toEqual(clear);
  expect(initialSshSecret({ ...tunneled, ssh: { ...tunneled.ssh!, hasSecret: false } }, "Key")).toEqual(clear);
  expect(isDirty(tunneled, form({ sshAuth: "Password" }, initialSshSecret(tunneled, "Password")))).toBe(true);
  expect(isDirty(tunneled, form({ sshAuth: "Key" }, initialSshSecret(tunneled, "Key")))).toBe(false);
});

it("sends the secret edit for password and key auth, and none for the agent", () => {
  const password = form({ sshAuth: "Password" }, { kind: "set", value: "hunter2" }).fields;
  expect(toSshDraft(password, { kind: "set", value: "hunter2" })).toMatchObject({ auth: { kind: "Password" }, secret: { Set: "hunter2" } });
  expect(toSshDraft(form({}).fields, clear)).toMatchObject({ auth: { kind: "Key", path: "~/.ssh/id_ed25519" }, secret: "Clear" });
  expect(toSshDraft(form({ sshAuth: "Agent" }).fields, keep)).toMatchObject({ auth: { kind: "Agent", socket: null }, secret: "Clear" });
  expect(toSshDraft(form({ sshAuth: "Agent", sshAgentSocket: " /tmp/agent.sock " }).fields, keep)).toMatchObject({
    auth: { kind: "Agent", socket: "/tmp/agent.sock" },
  });
});

it("needs a host, a port, a user, and a key path for key auth before a tunnel can dial", () => {
  expect(sshValid(initialFields(saved))).toBe(true);
  expect(sshValid(form({}).fields)).toBe(true);
  expect(sshValid(form({ sshHost: " " }).fields)).toBe(false);
  expect(sshValid(form({ sshPort: "70000" }).fields)).toBe(false);
  expect(sshValid(form({ sshUser: "" }).fields)).toBe(false);
  expect(sshValid(form({ sshKeyPath: "" }).fields)).toBe(false);
  expect(sshValid(form({ sshKeyPath: "", sshAuth: "Agent" }).fields)).toBe(true);
});

it("names how many saved connections could not be read, and says nothing when all could", () => {
  expect(unreadableConnectionsNotice(0)).toBeNull();
  expect(unreadableConnectionsNotice(1)).toBe("1 saved connection could not be read, so it is hidden. A newer version of L1 Data may have saved it.");
  expect(unreadableConnectionsNotice(2)).toBe("2 saved connections could not be read, so they are hidden. A newer version of L1 Data may have saved them.");
});
