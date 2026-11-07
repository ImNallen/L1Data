import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it } from "vitest";
import type { ChatState, ChatStatus, Entry, EntrySnapshot, ProbeOutcome, ProviderState } from "../../ipc/bindings";
import { useLive, type LiveData } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { AssistantPanel } from "./AssistantPanel";
import { ChatHistory } from "./ChatHistory";

const initialLive = { ...useLive.getInitialState() };
const initialWorkspace = { ...useWorkspace.getInitialState() };
const fullSettings = { ...initialWorkspace.settings, assistantAccess: "Full" } as const;

afterEach(() => {
  Object.assign(useLive.getInitialState(), initialLive);
  Object.assign(useWorkspace.getInitialState(), initialWorkspace);
});

const ID = "0b7d5a52-9f1e-4c55-9d43-3c1f3c6e2a10";
const DB = { connectionId: "c1", database: "shop" };

const CLAUDE: ProviderState = {
  provider: "Claude",
  rev: 2,
  info: {
    defaultName: "Claude",
    defaultBinary: "claude",
    configEnv: "CLAUDE_CONFIG_DIR",
    configDirDefault: "~/.claude",
    signInCommand: "claude auth login",
    ownedFlags: [],
  },
  ignoredFlags: [],
  checking: false,
  checkedAtMs: 1,
  outcome: {
    kind: "Ready",
    version: "2.1.301",
    plan: "Claude Max",
    models: [
      {
        id: "opus",
        label: "Opus 5.5",
        description: "",
        recommended: true,
        efforts: [
          { id: "high", label: "High" },
          { id: "max", label: "Max" },
        ],
      },
      { id: "haiku", label: "Haiku 4.5", description: "", recommended: false, efforts: [] },
    ],
  },
};

const snap = (seq: number, turn: number, entry: Entry): EntrySnapshot => ({ chatId: ID, seq, rev: 10 + seq, turn, entry });

const REPLY = [
  "This compares paid revenue per region:",
  "`FILTER` keeps both periods in one scan of [orders](https://example.com/x).",
  "| region | growth |\n|---|---|\n| EU-West | 4% |",
  "<b>raw</b> html stays out.",
].join("\n\n");

const FIRST_TURN = [
  snap(0, 0, {
    kind: "User",
    atMs: 1,
    text: "Which regions grew the most compared with the 90 days before?",
    context: { databaseKind: "Postgres", db: DB, connectionName: "prod-postgres", tabTitle: "orders_by_region.sql" },
    attachment: { tabTitle: "orders_by_region.sql", columns: ["region"], rows: [["EU-West"]], totalRows: 6 },
  }),
  snap(1, 0, {
    kind: "Tool",
    call: "toolu_1",
    tool: { kind: "RunQuery", sql: "select region from customers", purpose: "List regions", relations: ["customers"] },
    status: {
      kind: "Done",
      result: {
        kind: "Rows",
        columns: ["region"],
        rows: [["EU-West"], ["US-West"], ["APAC"], ["US-East"], ["LATAM"], ["EU-North"]],
        more: false,
        elapsedMs: 27,
      },
    },
  }),
  snap(2, 0, {
    kind: "Text",
    done: true,
    blocks: [
      { kind: "Markdown", text: REPLY },
      {
        kind: "Sql",
        sql: "SELECT c.region FROM orders o JOIN customers c ON c.id = o.customer_id;",
        relations: ["orders", "customers"],
        vetted: true,
        complete: true,
      },
    ],
  }),
  snap(3, 0, { kind: "End", atMs: 2, outcome: { kind: "Completed" }, suggestions: ["Show growth as a percentage", "Only EU regions"] }),
];

function render(status: ChatStatus, entries: EntrySnapshot[], outcome: ProbeOutcome | null = CLAUDE.outcome) {
  const chat: ChatState = {
    id: ID,
    rev: 20,
    title: "Which regions grew",
    updatedAtMs: 1,
    options: { provider: "Claude", model: "opus", effort: "high", permission: "AskFirst" },
    status,
    entryCount: entries.length,
  };
  const live: Partial<LiveData> = {
    chats: { [ID]: chat },
    transcripts: { [ID]: Object.fromEntries(entries.map((e) => [e.seq, e])) },
    providers: { Claude: { ...CLAUDE, outcome } },
  };
  Object.assign(useLive.getInitialState(), live);
  Object.assign(useWorkspace.getInitialState(), { settings: fullSettings, layout: { ...initialWorkspace.layout, assistantChat: ID } });
  return renderToStaticMarkup(<AssistantPanel />);
}

const text = (html: string | undefined) =>
  html
    ?.replace(/<\/?(?:span|code)[^>]*>/g, "")
    .replace(/<[^>]+>/g, " ")
    .replace(/&#x27;/g, "'")
    .replace(/\s+/g, " ")
    .trim();
const section = (html: string, id: string, next?: string) => {
  const at = html.indexOf(`data-testid="${id}"`);
  if (at < 0) return undefined;
  const end = next ? html.indexOf(`data-testid="${next}"`, at) : -1;
  return html.slice(html.lastIndexOf("<", at), end < 0 ? undefined : html.lastIndexOf("<", end));
};
const buttonText = (html: string, id: string) => text(new RegExp(`<button[^>]*data-testid="${id}"[^>]*>(.*?)</button>`).exec(html)?.[1]);

it("draws a finished turn as the mockup does: bubble and context, tool line, markdown with a SQL card, and suggestion chips", () => {
  const html = render({ kind: "Idle" }, FIRST_TURN);
  expect(text(section(html, "chat-user-0", "chat-tool-1"))).toBe(
    "Which regions grew the most compared with the 90 days before? prod-postgres / shop · orders_by_region.sql · 6 rows attached",
  );
  expect(text(section(html, "chat-tool-1", "chat-text-2"))).toContain("Ran query · 6 rows · 27 ms");
  const reply = section(html, "chat-text-2", "chat-end-3") ?? "";
  expect(reply).toContain('<code class="rounded-[4px] bg-pill');
  expect(reply).not.toContain("<a ");
  expect(reply).not.toContain("<b>");
  expect(text(reply)).toContain("FILTER keeps both periods in one scan of orders.");
  expect(reply).toMatch(/<table[^>]*>.*<th[^>]*>region<\/th>.*<td[^>]*>EU-West<\/td>/);
  expect(text(/data-testid="sql-card-uses"[^>]*>(.*?)<\/span>/.exec(reply)?.[1])).toBe("Uses orders, customers");
  expect(html).toContain('data-testid="chat-end-3" data-outcome="Completed"');
  expect(html).toContain('type="checkbox"');
  expect(text(html)).toContain("Show growth as a percentage");
  expect(text(html)).toContain("Only EU regions");
  expect(text(html)).toContain("Continue");
  expect(html).not.toContain("Thinking");
});

it("labels the pickers from the chat's options and the provider's models, and offers Send while idle", () => {
  const html = render({ kind: "Idle" }, FIRST_TURN);
  expect(buttonText(html, "assistant-model")).toBe("Opus 5.5");
  expect(html).toContain('data-testid="assistant-options"');
  expect(html).not.toContain('data-testid="assistant-effort"');
  expect(html).toContain('data-testid="assistant-send"');
  expect(html).not.toContain('data-testid="assistant-stop"');
  expect(html).not.toContain('data-testid="assistant-provider-banner"');
});

it("shows Starting while the CLI starts, then a waiting approval card, with Stop in place of Send", () => {
  const ask = FIRST_TURN.slice(0, 1);
  const starting = render({ kind: "Running", turn: 0, startedAtMs: 1, phase: "Starting" }, ask);
  expect(starting).toContain(">Starting Claude…</p>");
  expect([starting.includes('data-testid="assistant-stop"'), starting.includes('data-testid="assistant-send"')]).toEqual([true, false]);

  const waiting = render({ kind: "Running", turn: 0, startedAtMs: 1, phase: "AwaitingApproval" }, [
    ...ask,
    snap(1, 0, {
      kind: "Tool",
      call: "toolu_2",
      tool: { kind: "RunQuery", sql: "select dblink_exec('x')", purpose: "Check the link", relations: [] },
      status: { kind: "AwaitingApproval", approval: 7, reason: { kind: "Unvetted", functions: ["dblink_exec"] } },
    }),
  ]);
  const card = section(waiting, "chat-tool-1") ?? "";
  expect(text(card)).toContain(
    "Run this query? Check the link select dblink_exec('x') Calls functions L1 Data cannot verify as read-only: dblink_exec Reject Approve once",
  );
  expect([card.includes('data-testid="approval-run"'), card.includes('data-testid="approval-deny"'), waiting.includes("Thinking")]).toEqual([
    true,
    true,
    false,
  ]);
});

it("ends a failed turn with its error and the fix, and warns above the composer when the provider is signed out", () => {
  const failed = render(
    { kind: "Idle" },
    [...FIRST_TURN.slice(0, 1), snap(1, 0, { kind: "End", atMs: 2, outcome: { kind: "Failed", error: { kind: "SignedOut" } }, suggestions: [] })],
    {
      kind: "SignedOut",
      version: "2.1.301",
    },
  );
  expect(text(section(failed, "chat-end-1", "assistant-provider-banner"))).toBe("Claude is signed out. Run claude auth login in a terminal. Open settings");
  const banner = section(failed, "assistant-provider-banner", "assistant-input") ?? "";
  expect(text(banner)).toMatch(/^Claude is signed out\. Run claude auth login in a terminal, then refresh\b/);
  expect(banner).toMatch(/>refresh<\/button>\.<\/p><button[^>]*>Open settings<\/button>/);
});

it("dots the history button while any chat waits for approval", () => {
  const OTHER = "0b7d5a52-9f1e-4c55-9d43-3c1f3c6e2a11";
  const chat = (id: string, status: ChatStatus): ChatState => ({
    id,
    rev: 1,
    title: "t",
    updatedAtMs: 1,
    options: { provider: "Claude", model: "opus", effort: "high", permission: "AskFirst" },
    status,
    entryCount: 1,
  });
  const history = (status: ChatStatus) => {
    Object.assign(useLive.getInitialState(), { chats: { [ID]: chat(ID, { kind: "Idle" }), [OTHER]: chat(OTHER, status) } });
    const html = renderToStaticMarkup(<ChatHistory />);
    return [/aria-label="([^"]*)"/.exec(html)?.[1], html.includes('data-testid="assistant-history-waiting"')];
  };
  expect([
    history({ kind: "Running", turn: 0, startedAtMs: 1, phase: "AwaitingApproval" }),
    history({ kind: "Running", turn: 0, startedAtMs: 1, phase: "Working" }),
  ]).toEqual([
    ["Chat history, a chat is waiting for approval", true],
    ["Chat history", false],
  ]);
});

it("highlights SQL cards from saved SQL Server context after its connection is deleted", () => {
  const entries = [
    snap(0, 0, {
      kind: "User",
      atMs: 1,
      text: "Show top rows",
      context: { db: DB, databaseKind: "SqlServer", connectionName: "removed", tabTitle: "old" },
      attachment: null,
    }),
    snap(1, 0, {
      kind: "Text",
      done: true,
      blocks: [{ kind: "Sql", sql: "SELECT TOP 1 [odd name] FROM [dbo].[line items]", relations: [], vetted: false, complete: true }],
    }),
  ];
  const html = render({ kind: "Idle" }, entries);
  expect(html).toContain('class="text-keyword">TOP</span>');
  expect(html).toContain('data-testid="sql-open-new-tab"');
});
