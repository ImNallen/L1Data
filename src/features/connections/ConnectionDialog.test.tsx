// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import type { ConnectionConfig, ConnectionDraft } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { ConnectionDialog } from "./ConnectionDialog";

afterEach(() => {
  cleanup();
  clearMocks();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

const sqlServerUrl = "sqlserver://report:s3cret@db.example.net:1434/reporting";
const sqlServerDraft: ConnectionDraft = {
  databaseKind: "SqlServer",
  id: null,
  name: "",
  host: "db.example.net",
  port: 1434,
  database: "reporting",
  user: "report",
  sslMode: "VerifyFull",
  caCertPath: null,
  password: { Set: "s3cret" },
  color: null,
  ssh: null,
  environment: null,
  sqlServerAuth: "SqlLogin",
};

function openNew() {
  useWorkspace.setState({ overlay: { kind: "connection", editing: null } });
  render(<ConnectionDialog />);
}

function openEditing(config: ConnectionConfig) {
  useWorkspace.setState({ configs: { [config.id]: config }, overlay: { kind: "connection", editing: config.id } });
  render(<ConnectionDialog />);
}

const value = (testId: string) => (screen.getByTestId(testId) as HTMLInputElement).value;
const checked = (testId: string) => screen.getByTestId(testId).getAttribute("aria-checked") === "true";

it("starts a new connection at the kind chooser with the URL field focused", () => {
  openNew();
  expect(screen.getByRole("heading", { name: "Connect a database" })).toBeTruthy();
  expect(screen.getByTestId("conn-kind-postgres")).toBeTruthy();
  expect(screen.getByTestId("conn-kind-sqlserver")).toBeTruthy();
  expect(screen.queryByTestId("conn-host")).toBeNull();
  expect(document.activeElement).toBe(screen.getByTestId("conn-url"));
});

it("opens each kind's form with its defaults, and Back returns to the chooser", async () => {
  const user = userEvent.setup();
  openNew();
  await user.click(screen.getByTestId("conn-kind-sqlserver"));
  expect(screen.getByRole("heading", { name: "New SQL Server connection" })).toBeTruthy();
  expect(value("conn-port")).toBe("1433");
  expect(value("conn-database")).toBe("master");
  expect(screen.queryByTestId("conn-ssl")).toBeNull();

  await user.click(screen.getByTestId("conn-back"));
  expect(screen.queryByTestId("discard-dialog")).toBeNull();
  await user.click(screen.getByTestId("conn-kind-postgres"));
  expect(screen.getByRole("heading", { name: "New PostgreSQL connection" })).toBeTruthy();
  expect(value("conn-port")).toBe("5432");
  expect(screen.queryByTestId("conn-auth")).toBeNull();
});

it("asks before Back drops a changed form, and starts the next form fresh", async () => {
  const user = userEvent.setup();
  openNew();
  await user.click(screen.getByTestId("conn-kind-postgres"));
  await user.clear(screen.getByTestId("conn-host"));
  await user.type(screen.getByTestId("conn-host"), "db.internal");

  await user.click(screen.getByTestId("conn-back"));
  await user.click(await screen.findByTestId("discard-cancel"));
  expect(value("conn-host")).toBe("db.internal");

  await user.click(screen.getByTestId("conn-back"));
  await user.click(await screen.findByTestId("discard-confirm"));
  await user.click(await screen.findByTestId("conn-kind-postgres"));
  expect(value("conn-host")).toBe("localhost");
});

it("opens the SQL Server form filled in from a URL pasted in the chooser", async () => {
  const user = userEvent.setup();
  const parsed: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd !== "connection_parse_url") return null;
    parsed.push(args);
    return sqlServerDraft;
  });
  openNew();
  await user.paste(sqlServerUrl);

  expect(await screen.findByRole("heading", { name: "New SQL Server connection" })).toBeTruthy();
  expect(parsed).toContainEqual({ url: sqlServerUrl });
  expect(value("conn-url")).toBe(sqlServerUrl);
  expect(value("conn-host")).toBe("db.example.net");
  expect(value("conn-port")).toBe("1434");
  expect(value("conn-database")).toBe("reporting");
  expect(value("conn-user")).toBe("report");
  expect(value("conn-password")).toBe("s3cret");
  expect(checked("conn-encryption-VerifyFull")).toBe(true);
});

it("waits for Enter before a typed URL leaves the chooser", async () => {
  const user = userEvent.setup();
  mockIPC((cmd) => (cmd === "connection_parse_url" ? sqlServerDraft : null));
  openNew();
  await user.type(screen.getByTestId("conn-url"), sqlServerUrl);
  expect(screen.getByTestId("conn-kind-sqlserver")).toBeTruthy();

  await user.keyboard("{Enter}");
  expect(await screen.findByRole("heading", { name: "New SQL Server connection" })).toBeTruthy();
  expect(value("conn-host")).toBe("db.example.net");
});

it("shows why a URL does not parse and stays on the chooser", async () => {
  const user = userEvent.setup();
  mockIPC((cmd) => {
    if (cmd === "connection_parse_url") throw "Unsupported scheme mysql";
    return null;
  });
  openNew();
  await user.paste("mysql://db/shop");
  expect(await screen.findByText("Unsupported scheme mysql")).toBeTruthy();
  expect(screen.getByTestId("conn-url").getAttribute("aria-invalid")).toBe("true");
  expect(screen.getByTestId("conn-kind-postgres")).toBeTruthy();
});

it("refuses a URL of the other kind in a form", async () => {
  const user = userEvent.setup();
  mockIPC((cmd) => (cmd === "connection_parse_url" ? sqlServerDraft : null));
  openNew();
  await user.click(screen.getByTestId("conn-kind-postgres"));
  await user.paste(sqlServerUrl);
  expect(await screen.findByText("That is a SQL Server URL. Go back to choose SQL Server.")).toBeTruthy();
  expect(value("conn-host")).toBe("localhost");
  expect(value("conn-port")).toBe("5432");
});

it("signs in with Entra ID without a login or password, over Verify full only", async () => {
  const user = userEvent.setup();
  openNew();
  await user.click(screen.getByTestId("conn-kind-sqlserver"));
  await user.click(screen.getByTestId("conn-encryption-Require"));
  expect(checked("conn-encryption-Require")).toBe(true);

  await user.click(screen.getByTestId("conn-auth-EntraAzureCli"));
  expect(screen.queryByTestId("conn-user")).toBeNull();
  expect(screen.queryByTestId("conn-password")).toBeNull();
  expect(screen.getByTestId("conn-entra").textContent).toContain("az login");
  expect(checked("conn-encryption-VerifyFull")).toBe(true);
  expect((screen.getByTestId("conn-encryption-Require") as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByTestId("conn-save") as HTMLButtonElement).disabled).toBe(false);

  await user.click(screen.getByTestId("conn-auth-SqlLogin"));
  expect(screen.getByTestId("conn-user")).toBeTruthy();
  expect((screen.getByTestId("conn-encryption-Require") as HTMLButtonElement).disabled).toBe(false);
});

it("blocks saving an Entra ID connection that a URL set to Require", async () => {
  const user = userEvent.setup();
  mockIPC((cmd) => (cmd === "connection_parse_url" ? { ...sqlServerDraft, sqlServerAuth: "EntraAzureCli", sslMode: "Require", password: "Keep" } : null));
  openNew();
  await user.paste("sqlserver://db.example.net?authentication=ActiveDirectoryDefault&encrypt=true");
  expect(await screen.findByTestId("conn-ssl-entra")).toBeTruthy();
  expect((screen.getByTestId("conn-save") as HTMLButtonElement).disabled).toBe(true);

  await user.click(screen.getByTestId("conn-encryption-VerifyFull"));
  expect(screen.queryByTestId("conn-ssl-entra")).toBeNull();
  expect((screen.getByTestId("conn-save") as HTMLButtonElement).disabled).toBe(false);
});

it("shows a sign-in method from a newer version as unselected and refuses to save it", () => {
  openEditing({ ...connectionConfig("c1", "Azure"), databaseKind: "SqlServer", sslMode: "VerifyFull", sqlServerAuth: "Unknown" });
  expect(checked("conn-auth-SqlLogin")).toBe(false);
  expect(checked("conn-auth-EntraAzureCli")).toBe(false);
  expect(screen.getByText(/A newer version of L1 Data saved this sign-in method/)).toBeTruthy();
  expect((screen.getByTestId("conn-save") as HTMLButtonElement).disabled).toBe(true);
});

it("edits a saved connection with its name focused, no Back or URL, and its CA certificate shown", () => {
  openEditing({ ...connectionConfig("c1", "Reporting"), caCertPath: "/tmp/database-ca.pem" });
  expect(screen.getByRole("heading", { name: "Reporting" })).toBeTruthy();
  expect(screen.queryByTestId("conn-back")).toBeNull();
  expect(screen.queryByTestId("conn-url")).toBeNull();
  expect(document.activeElement).toBe(screen.getByTestId("conn-name"));
  expect(screen.getByTestId("conn-ca-cert").closest("details")?.open).toBe(true);
  expect(value("conn-ca-cert")).toBe("/tmp/database-ca.pem");
  expect(screen.getByTestId("conn-save").textContent).toBe("Save");
});

it("keeps network values while the disclosure is collapsed, and summarizes what is set", async () => {
  const user = userEvent.setup();
  openNew();
  await user.click(screen.getByTestId("conn-kind-postgres"));
  const summary = screen.getByTestId("conn-network-summary");
  const details = summary.closest("details");
  expect(details?.open).toBe(false);
  expect(summary.textContent).toBe("TLS Prefer · No tunnel");

  await user.click(summary);
  await waitFor(() => expect(details?.open).toBe(true));
  await user.type(screen.getByTestId("conn-ca-cert"), "/tmp/database-ca.pem");
  await user.click(screen.getByTestId("conn-ssh-enabled"));
  expect(summary.textContent).toBe("TLS Prefer · SSH not set up");
  expect(summary.className).toContain("text-danger");
  expect((screen.getByTestId("conn-save") as HTMLButtonElement).disabled).toBe(true);

  await user.type(screen.getByTestId("conn-ssh-host"), "bastion.example.com");
  await user.type(screen.getByTestId("conn-ssh-user"), "deploy");
  expect(summary.textContent).toBe("TLS Prefer · SSH via bastion.example.com");
  expect(summary.className).not.toContain("text-danger");

  await user.click(summary);
  await waitFor(() => expect(details?.open).toBe(false));
  await user.click(summary);
  await waitFor(() => expect(details?.open).toBe(true));
  expect(value("conn-ca-cert")).toBe("/tmp/database-ca.pem");
  expect(value("conn-ssh-host")).toBe("bastion.example.com");
});

it("keeps a typed SSH password when its sign-in method is chosen again", async () => {
  const user = userEvent.setup();
  openNew();
  await user.click(screen.getByTestId("conn-kind-postgres"));
  await user.click(screen.getByTestId("conn-network-summary"));
  await user.click(screen.getByTestId("conn-ssh-enabled"));
  await user.type(screen.getByTestId("conn-ssh-secret"), "hunter2");
  await user.click(screen.getByTestId("conn-ssh-auth-Password"));
  expect(value("conn-ssh-secret")).toBe("hunter2");
});

it("picks an environment with the arrow keys and says what it does to Run", async () => {
  const user = userEvent.setup();
  openNew();
  await user.click(screen.getByTestId("conn-kind-postgres"));
  expect(checked("conn-environment-none")).toBe(true);
  screen.getByTestId("conn-environment-none").focus();
  await user.keyboard("{ArrowRight}{ArrowRight}");
  expect(checked("conn-environment-Staging")).toBe(true);
  expect(document.activeElement).toBe(screen.getByTestId("conn-environment-Staging"));
  expect(screen.getByText("Run turns yellow.")).toBeTruthy();
  await user.click(screen.getByTestId("conn-environment-Production"));
  expect(screen.getByText("Run turns red, and a run that writes asks first.")).toBeTruthy();
});
