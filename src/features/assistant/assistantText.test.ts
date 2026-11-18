import { expect, it } from "vitest";
import type { AgentError, ProbeOutcome, ProviderState } from "../../ipc/bindings";
import type { UserEntry } from "../../state/transcript";
import { agentErrorText, checkedAgo, contextChip, providerBanner, providerStatus } from "./assistantText";

const NOW = Date.UTC(2026, 9, 16, 12, 0, 0);
const SEC = 1000;
const MIN = 60 * SEC;
const HOUR = 60 * MIN;

const INFO = {
  defaultName: "Claude",
  defaultBinary: "claude",
  configEnv: "CLAUDE_CONFIG_DIR",
  configDirDefault: "~/.claude",
  signInCommand: "claude auth login",
  ownedFlags: ["--model", "--resume", "--output-format"],
};

function provider(outcome: ProbeOutcome | null, extra: Partial<ProviderState> = {}): ProviderState {
  return { provider: "Claude", rev: 1, info: INFO, ignoredFlags: [], checking: false, checkedAtMs: NOW - 90 * SEC, outcome, ...extra };
}

it("says how long ago the newest check finished, in whole minutes, hours, or days", () => {
  const at = (ago: number) => checkedAgo([provider(null, { checkedAtMs: NOW - ago })], NOW);
  expect([at(0), at(59 * SEC), at(60 * SEC), at(59 * MIN + 59 * SEC), at(HOUR), at(23 * HOUR + 59 * MIN), at(24 * HOUR), at(9 * 24 * HOUR)]).toEqual([
    "Checked just now",
    "Checked just now",
    "Checked 1m ago",
    "Checked 59m ago",
    "Checked 1h ago",
    "Checked 23h ago",
    "Checked 1d ago",
    "Checked 9d ago",
  ]);
});

it("reads the newest of several checks, says Checking while any probe runs, and admits when none has finished", () => {
  const old = provider(null, { checkedAtMs: NOW - 3 * HOUR });
  const recent = provider(null, { checkedAtMs: NOW - 4 * MIN });
  expect(checkedAgo([old, recent], NOW)).toBe("Checked 4m ago");
  expect(checkedAgo([old, { ...recent, checking: true }], NOW)).toBe("Checking…");
  expect(checkedAgo([provider(null, { checkedAtMs: null })], NOW)).toBe("Not checked yet");
  expect(checkedAgo([], NOW)).toBe("Not checked yet");
});

it("states each probe outcome under the provider's name, and keeps it on screen while the next probe runs", () => {
  const ready = (plan: string | null): ProbeOutcome => ({ kind: "Ready", version: "2.1.301", plan, models: [] });
  expect(
    [
      provider(ready("Claude Max")),
      provider(ready(null)),
      provider({ kind: "SignedOut", version: "2.1.301" }),
      provider({ kind: "NotFound", binary: "claude on PATH" }),
      provider({ kind: "Broken", version: "2.1.301", message: "auth status timed out after 5 s" }),
      provider(null, { checking: true, checkedAtMs: null }),
      provider(null, { checkedAtMs: null }),
      provider(ready("Claude Max"), { checking: true }),
      undefined,
    ].map(providerStatus),
  ).toEqual([
    { text: "Signed in · Claude Max", tone: "ok" },
    { text: "Signed in", tone: "ok" },
    { text: "Not signed in", tone: "warn" },
    { text: "Not found", tone: "error" },
    { text: "auth status timed out after 5 s", tone: "error" },
    { text: "Checking…", tone: "muted" },
    { text: "Not checked yet", tone: "muted" },
    { text: "Signed in · Claude Max", tone: "ok" },
    { text: "Not checked yet", tone: "muted" },
  ]);
});

const VOICE = { name: "Claude", signInCommand: "claude auth login" };

it("states every way a turn can fail, with the fix it points at", () => {
  const errors: AgentError[] = [
    { kind: "NotFound", binary: "claude on PATH" },
    { kind: "SignedOut" },
    { kind: "Exited", code: 1, detail: "Error: session not found\n" },
    { kind: "Exited", code: null, detail: "" },
    { kind: "Provider", message: "Rate limited. Try again in 2 minutes." },
  ];
  expect(errors.map((e) => agentErrorText(e, VOICE))).toEqual([
    { text: "Claude CLI not found at claude on PATH.", action: "settings" },
    { text: "Claude is signed out. Run claude auth login in a terminal.", action: "settings" },
    { text: "Claude exited (code 1): Error: session not found", action: "retry" },
    { text: "Claude exited.", action: "retry" },
    { text: "Rate limited. Try again in 2 minutes.", action: "retry" },
  ]);
});

const user = (context: UserEntry["context"], attachment: UserEntry["attachment"] = null): UserEntry => ({
  kind: "User",
  atMs: 1,
  text: "hi",
  context,
  attachment,
});

it("chips a sent message with its connection, database, tab, and attached rows, skipping what it lacks", () => {
  const db = { connectionId: "c1", database: "shop" };
  const rows = { tabTitle: "orders_by_region.sql", ask: null, columns: ["region"], rows: [["EU-West"]], totalRows: 6 };
  expect([
    contextChip(user({ databaseKind: "Postgres", db, connectionName: "prod-postgres", tabTitle: "orders_by_region.sql", ask: null }, rows)),
    contextChip(user({ databaseKind: "Postgres", db, connectionName: null, tabTitle: "Untitled-1.sql", ask: null })),
    contextChip(user({ databaseKind: null, db: null, connectionName: null, tabTitle: "", ask: null })),
    contextChip(user(null)),
  ]).toEqual(["prod-postgres / shop · orders_by_region.sql · 6 rows attached", "shop · Untitled-1.sql", null, null]);
});

it("raises a banner when the chat's provider is off or failed its last check, and stays quiet while it is ready or unchecked", () => {
  const config = { provider: "Claude", enabled: true, displayName: "Claude", binaryPath: "", configDir: "", launchArgs: "" } as const;
  expect([
    providerBanner({ ...config, enabled: false }, provider({ kind: "Ready", version: "2.1.301", plan: null, models: [] })),
    providerBanner(config, provider({ kind: "NotFound", binary: "/opt/bin/claude" })),
    providerBanner(config, provider({ kind: "SignedOut", version: "2.1.301" })),
    providerBanner(config, provider({ kind: "Broken", version: null, message: "--version timed out" })),
    providerBanner(config, provider({ kind: "Ready", version: "2.1.301", plan: null, models: [] })),
    providerBanner(config, provider(null)),
    providerBanner(undefined, undefined),
  ]).toEqual([
    { kind: "Off", name: "Claude" },
    { kind: "NotFound", name: "Claude", binary: "/opt/bin/claude" },
    { kind: "SignedOut", name: "Claude", command: "claude auth login" },
    { kind: "Broken", name: "Claude", message: "--version timed out" },
    null,
    null,
    null,
  ]);
});
