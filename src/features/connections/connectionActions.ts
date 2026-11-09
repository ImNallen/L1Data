import { commands, type ConnectError, type ConnectionId, type DatabaseRef, type SshError, type ViewEdit } from "../../ipc/bindings";
import { catalogErrorText, connectErrorText, disconnectErrorText } from "../../ipc/errors";
import { confirm, useWorkspace } from "../../state/workspace";
import { plural } from "../editor/runActions";
import { closeCells, holdsTransaction } from "../notebooks/notebookActions";
import { keepNotebook } from "../notebooks/notebookFiles";

const ws = () => useWorkspace.getState();

export type UnknownHostKey = Extract<SshError, { kind: "HostKeyUnknown" }>;

/** The bastion key a Test or Connect can trust and retry with. Changed and revoked keys are not offered. */
export function unknownHostKey(e: ConnectError): UnknownHostKey | null {
  return e.kind === "Ssh" && e.error.kind === "HostKeyUnknown" ? e.error : null;
}

export async function connect(id: ConnectionId): Promise<void> {
  const res = await commands.connectionConnect(id);
  if (res.status === "ok") return;
  const name = ws().configs[id]?.name ?? "Connection";
  const key = unknownHostKey(res.error);
  if (!key) {
    ws().notify(`${name}: ${connectErrorText(res.error)}`);
    return;
  }
  const trust = await confirm({
    title: `Trust the host key of ${key.host}:${key.port}?`,
    body: `${name} connects through this SSH server, which offered an unknown ${key.algorithm} key ${key.fingerprint}. Trust it only if it matches the server's key.`,
    confirmLabel: "Trust and connect",
  });
  if (!trust) return;
  const trusted = await commands.sshTrustHostKey(key.trust);
  if (trusted.status === "error") ws().notify(`${name}: SSH: ${trusted.error}`);
  else await connect(id);
}

export async function disconnect(id: ConnectionId): Promise<void> {
  const res = await commands.connectionDisconnect(id, false);
  if (res.status === "ok") return;
  if (res.error.kind !== "OpenTransactions") {
    ws().notify(disconnectErrorText(res.error));
    return;
  }
  const rollBack = await confirm({
    title: "Disconnect with open transactions?",
    body: `${disconnectErrorText(res.error)} Disconnecting rolls it back.`,
    confirmLabel: "Roll back and disconnect",
  });
  if (rollBack) await commands.connectionDisconnect(id, true);
}

/**
 * Names the notebooks on the connection's databases, which go with it. Each unsaved file notebook then asks once to
 * save, and a Cancel there keeps the connection. Their cells close first, rolling back open transactions.
 */
export async function confirmDelete(id: ConnectionId): Promise<void> {
  const name = ws().configs[id]?.name ?? "this connection";
  const notebooks = Object.values(ws().notebooks)
    .filter((n) => n.db?.connectionId === id)
    .sort((a, b) => b.shownAtMs - a.shownAtMs);
  const cells = notebooks.flatMap((n) => n.cells);
  const ok = await confirm({
    title: `Delete ${name}?`,
    body: notebooks.length > 0 ? "Removes the connection, its password, and these notebooks:" : "Removes the connection and its password.",
    rows: notebooks.map((n) => ({ label: n.title, detail: `${n.db?.database} · ${plural(n.cells.length, "cell")}` })),
    note: holdsTransaction(cells) ? "Any open transactions are rolled back." : undefined,
    confirmLabel: "Delete",
  });
  if (!ok) return;
  for (const n of notebooks) if (n.file && (await keepNotebook(n.id)) === false) return;
  if (!(await closeCells(cells))) return;
  const res = await commands.connectionDelete(id);
  if (res.status === "error") ws().notify(disconnectErrorText(res.error));
  else ws().removeConfig(id);
}

export async function refreshCatalog(db: DatabaseRef): Promise<void> {
  const res = await commands.catalogRefresh(db);
  if (res.status === "error") ws().notify(res.error.kind === "Failed" ? res.error.message : "Connect first to refresh the catalog.");
}

export async function refreshConnection(id: ConnectionId): Promise<void> {
  const res = await commands.connectionRefresh(id);
  if (res.status === "error") ws().notify(catalogErrorText(res.error));
}

export async function editView(id: ConnectionId, edit: ViewEdit): Promise<void> {
  const res = await commands.connectionEditView(id, edit);
  if (res.status === "error") ws().notify(catalogErrorText(res.error));
}

export function probeDatabases(id: ConnectionId, names: string[]): void {
  void commands.connectionProbeDatabases(id, names).catch((e: unknown) => ws().notify(String(e)));
}
