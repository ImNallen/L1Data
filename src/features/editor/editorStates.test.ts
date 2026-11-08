// @vitest-environment jsdom
import { syntaxTree } from "@codemirror/language";
import {
  acceptCompletion,
  CompletionContext,
  completionStatus,
  currentCompletions,
  moveCompletionSelection,
  type CompletionResult,
} from "@codemirror/autocomplete";
import { EditorView } from "@codemirror/view";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import type { Catalog, CompletionContext as EngineContext, DatabaseKind, Expect, KeyInfo, ScopeRelation } from "../../ipc/bindings";
import { buildCatalogView } from "../../state/catalog";
import { useRelationDetail } from "../../state/relationDetail";
import { shopCatalog } from "../../testing/shopCatalog";
import { completionOptions } from "./completion";
import {
  configureSql,
  docOf,
  dropEditor,
  editorStateOf,
  flashRanges,
  insertAtCursor,
  moveCursor,
  onBuffersChanged,
  replaceDoc,
  seedEditor,
} from "./editorStates";

afterEach(() => {
  dropEditor("t1");
  clearMocks();
  useRelationDetail.setState({ entries: {} });
});

type Reply = Extract<EngineContext, { kind: "Complete" }>;
const table = (reference: string, name: string, schema: string | null = null, columnAliases: string[] = []): ScopeRelation => ({
  reference,
  source: { kind: "Table", schema, name },
  columnAliases,
});
const reply = (expect: Expect, relations: ScopeRelation[] = [], more: Partial<Reply> = {}): Reply => ({
  kind: "Complete",
  from: 0,
  expect,
  qualifier: null,
  relations,
  ctes: [],
  keywords: [],
  functions: [],
  ...more,
});

function foreignKey(columns: string[], refTable: string, refColumns: string[]): KeyInfo {
  return { name: `fk_${refTable}`, columns, kind: { kind: "Foreign", refSchema: "public", refTable, refColumns, onDelete: "NoAction", onUpdate: "NoAction" } };
}

const ORDER_KEYS = [foreignKey(["customer_id"], "customers", ["id"]), foreignKey(["customer_id", "ship_address_id"], "addresses", ["customer_id", "id"])];

type Setup = { keys?: Record<number, KeyInfo[]>; explicit?: boolean; catalog?: Catalog; kind?: DatabaseKind };

async function completeAtEnd(
  text: string,
  engine: Reply | { kind: "None" },
  { keys = {}, explicit = true, catalog = shopCatalog("c1", 1), kind = catalog.databaseKind }: Setup = {},
): Promise<{ labels: string[]; applies: unknown[]; sent: unknown[] }> {
  const sent: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "completion_context") {
      sent.push(args);
      return engine.kind === "Complete" ? { ...engine, from: engine.from || text.length } : engine;
    }
    if (cmd === "relation_detail") {
      const { db, oid } = args as { db: { connectionId: string; database: string }; oid: number };
      return { db, oid, catalogVersion: 1, defaults: [], keys: keys[oid] ?? [], indexes: [], triggers: [] };
    }
    return null;
  });
  seedEditor("t1", text);
  configureSql("t1", buildCatalogView(catalog), kind);
  const state = editorStateOf("t1");
  const pos = state.doc.length;
  const context = new CompletionContext(state, pos, explicit);
  const labels: string[] = [];
  const applies: unknown[] = [];
  for (const source of state.languageDataAt<(c: CompletionContext) => Promise<CompletionResult | null> | CompletionResult | null>("autocomplete", pos)) {
    const result = await source(context);
    labels.push(...(result?.options.map((o) => o.label) ?? []));
    applies.push(...(result?.options.map((o) => o.apply ?? o.label) ?? []));
  }
  return { labels, applies, sent };
}

const oidOf = (name: string) => shopCatalog("c1", 1).schemas[1]!.relations.find((r) => r.name === name)!.oid;

it("completes the columns of an aliased table in the default schema", async () => {
  const { labels, sent } = await completeAtEnd(
    "SELECT * FROM orders o WHERE o.",
    reply({ kind: "Expression" }, [table("o", "orders")], { qualifier: { kind: "Relation", index: 0 } }),
  );
  expect(labels).toEqual(["id", "customer_id", "status", "total", "created_at"]);
  expect(sent).toEqual([{ text: "SELECT * FROM orders o WHERE o.", cursor: 31, databaseKind: "Postgres" }]);
});

it("completes the columns of an aliased table in another schema", async () => {
  const { labels } = await completeAtEnd(
    "SELECT * FROM lab.type_zoo z WHERE z.",
    reply({ kind: "Expression" }, [table("z", "type_zoo", "lab")], { qualifier: { kind: "Relation", index: 0 } }),
  );
  expect(labels).toEqual(["id", "tstz", "m"]);
});

it("offers only relations and CTEs where a relation stands", async () => {
  const { labels } = await completeAtEnd("WITH recent AS (SELECT 1) SELECT * FROM ", reply({ kind: "Relation" }, [], { ctes: ["recent"], keywords: [] }));
  expect(labels.slice(0, 2)).toEqual(["recent", "addresses"]);
  expect(labels).toContain("lab.type_zoo");
  expect(labels).not.toContain("id");
  expect(labels).not.toContain("SELECT");
});

it("offers columns, catalog functions with their arguments, built-ins, and the keywords that can follow in an expression", async () => {
  const { labels } = await completeAtEnd(
    "SELECT * FROM orders WHERE ",
    reply({ kind: "Expression" }, [table("orders", "orders")], { functions: ["date_trunc"], keywords: ["AND", "ORDER BY"] }),
  );
  expect(labels).toEqual([
    "id",
    "customer_id",
    "status",
    "total",
    "created_at",
    "orders",
    "apply_coupon",
    "customer_ltv",
    "order_count",
    "slow_echo",
    "touch_order",
    "date_trunc",
    "AND",
    "ORDER BY",
  ]);
});

it("offers the foreign key join condition first, with the joined relation's columns on the left", async () => {
  const text = "select * from orders o join customers c on ";
  const { labels } = await completeAtEnd(text, reply({ kind: "JoinCondition", target: 1 }, [table("o", "orders"), table("c", "customers")]), {
    keys: { [oidOf("orders")]: ORDER_KEYS },
    explicit: false,
  });
  expect(labels[0]).toBe("c.id = o.customer_id");
  expect(labels).toContain("email");
});

it("joins every column of a composite foreign key with AND, and leaves out a key the catalog lacks a column for", async () => {
  const catalog = shopCatalog("c1", 1);
  const orders = catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  orders.columns.push({ ...orders.columns[1]!, attnum: 9, name: "ship_address_id", quoted: "ship_address_id" });
  const joinAddresses = reply({ kind: "JoinCondition", target: 1 }, [table("o", "orders"), table("a", "addresses")]);
  const { labels } = await completeAtEnd("select * from orders o join addresses a on ", joinAddresses, { keys: { [oidOf("orders")]: ORDER_KEYS }, catalog });
  expect(labels[0]).toBe("a.customer_id = o.customer_id AND a.id = o.ship_address_id");
  const stale = await completeAtEnd("select * from orders o join addresses a on ", joinAddresses, { keys: { [oidOf("orders")]: ORDER_KEYS } });
  expect(stale.labels.some((l) => l.includes(" = "))).toBe(false);
});

it("reads a foreign key from the joined relation to one already in scope", async () => {
  const { labels } = await completeAtEnd(
    "select * from customers c join orders o on ",
    reply({ kind: "JoinCondition", target: 1 }, [table("c", "customers"), table("o", "orders")]),
    { keys: { [oidOf("orders")]: ORDER_KEYS } },
  );
  expect(labels[0]).toBe("o.customer_id = c.id");
});

it("leaves an alias being typed alone, and lists the keywords only on request", async () => {
  const alias = reply({ kind: "Alias" }, [table("orders", "orders")], { keywords: ["ORDER BY", "WHERE"] });
  expect((await completeAtEnd("select * from orders o", alias, { explicit: false })).labels).toEqual([]);
  expect((await completeAtEnd("select * from orders o", alias)).labels).toEqual(["ORDER BY", "WHERE"]);
});

it("opens the keywords unasked when the word after a relation starts a reserved keyword", async () => {
  const text = "select * from orders wh";
  const keyword = reply({ kind: "Keyword" }, [table("orders", "orders")], { keywords: ["WHERE", "ORDER BY"], from: text.length - 2 });
  expect((await completeAtEnd(text, keyword, { explicit: false })).labels).toEqual(["WHERE", "ORDER BY"]);
});

it("stays closed after a space unless the engine says a join condition starts there", async () => {
  expect((await completeAtEnd("select * from orders o ", reply({ kind: "Keyword" }, [], { keywords: ["WHERE"] }), { explicit: false })).labels).toEqual([]);
  expect((await completeAtEnd("select 'abc", { kind: "None" })).labels).toEqual([]);
});

it("sends the SQL Server kind and offers the expansion of a derived relation", async () => {
  const derived: ScopeRelation = { reference: "r", source: { kind: "Derived", columns: ["n"], star: [{ schema: null, name: "coupons" }] }, columnAliases: [] };
  seedEditor("t1", "");
  const sent: unknown[] = [];
  mockIPC((_cmd, args) => {
    sent.push(args);
    return reply({ kind: "Expression" }, [derived], { qualifier: { kind: "Relation", index: 0 }, from: 2 });
  });
  configureSql("t1", buildCatalogView(shopCatalog("c1", 1)), "SqlServer");
  insertAtCursor("t1", "r.");
  const state = editorStateOf("t1");
  const [source] = state.languageDataAt<(c: CompletionContext) => Promise<CompletionResult | null>>("autocomplete", 2);
  const result = await source!(new CompletionContext(state, 2, false));
  expect(result?.options.map((o) => o.label)).toEqual(["n", "code", "percent_off", "expires_at"]);
  expect(sent).toEqual([{ text: "r.", cursor: 2, databaseKind: "SqlServer" }]);
});

it("names the first columns as a column alias list renames them, also in a join condition", async () => {
  const renamed = table("o", "orders", null, ["oid", "cid"]);
  const qualified = reply({ kind: "Expression" }, [renamed], { qualifier: { kind: "Relation", index: 0 } });
  expect((await completeAtEnd("select * from orders o(oid, cid) where o.", qualified)).labels).toEqual(["oid", "cid", "status", "total", "created_at"]);
  const unnest: ScopeRelation = { reference: "u", source: { kind: "Derived", columns: [], star: [] }, columnAliases: ["x"] };
  const fromUnnest = reply({ kind: "Expression" }, [unnest], { qualifier: { kind: "Relation", index: 0 } });
  expect((await completeAtEnd("select * from unnest(array[1]) u(x) where u.", fromUnnest)).labels).toEqual(["x"]);
  const join = reply({ kind: "JoinCondition", target: 1 }, [renamed, table("c", "customers")]);
  const { labels } = await completeAtEnd("select * from orders o(oid, cid) join customers c on ", join, { keys: { [oidOf("orders")]: ORDER_KEYS } });
  expect(labels[0]).toBe("c.id = o.cid");
});

it("inserts each catalog name as the engine quotes it", async () => {
  const catalog = shopCatalog("c1", 1);
  const orders = catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  Object.assign(orders, { name: "Orders", quoted: '"Orders"' });
  Object.assign(orders.columns[2]!, { name: "order", quoted: '"order"' });
  const qualified = reply({ kind: "Expression" }, [table("o", "Orders")], { qualifier: { kind: "Relation", index: 0 } });
  const { labels, applies } = await completeAtEnd('select * from "Orders" o where o.', qualified, { catalog });
  expect(applies[labels.indexOf("order")]).toBe('"order"');
  const relations = await completeAtEnd("select * from ", reply({ kind: "Relation" }), { catalog });
  expect(relations.applies[relations.labels.indexOf("Orders")]).toBe('"Orders"');
});

it("matches a name in another case only where the server would", async () => {
  const catalog = shopCatalog("c1", 1);
  catalog.schemas[1]!.relations.find((r) => r.name === "orders")!.name = "Orders";
  const qualified = reply({ kind: "Expression" }, [table("o", "orders")], { qualifier: { kind: "Relation", index: 0 } });
  expect((await completeAtEnd("select * from orders o where o.", qualified, { catalog })).labels).toEqual([]);
  const msCatalog = { ...catalog, databaseKind: "SqlServer" as const };
  expect((await completeAtEnd("SELECT * FROM orders o WHERE o.", qualified, { catalog: msCatalog })).labels).toContain("customer_id");
});

it("opens the list for a word typed while a declined request was out", async () => {
  const pending: (() => void)[] = [];
  mockIPC(async (cmd, args) => {
    if (cmd !== "completion_context") return null;
    const { text } = args as { text: string };
    await new Promise<void>((resolve) => pending.push(resolve));
    const typed = text.slice(text.lastIndexOf(" ") + 1);
    return reply(typed.length < 2 ? { kind: "Alias" } : { kind: "Keyword" }, [], { keywords: ["WHERE"], from: text.length - typed.length });
  });
  seedEditor("t1", "select * from orders ");
  configureSql("t1", buildCatalogView(shopCatalog("c1", 1)));
  const view = new EditorView({ state: editorStateOf("t1"), parent: document.body });
  try {
    const type = (text: string) =>
      view.dispatch({
        changes: { from: view.state.doc.length, insert: text },
        selection: { anchor: view.state.doc.length + text.length },
        userEvent: "input.type",
      });
    const settle = () => new Promise((resolve) => setTimeout(resolve, 150));
    type("w");
    await settle();
    type("h");
    pending.shift()!();
    await settle();
    pending.shift()?.();
    await settle();
    expect(currentCompletions(view.state).map((c) => c.label)).toEqual(["wh", "WHERE"]);
  } finally {
    view.destroy();
  }
});

function mountWith(text: string, answer: (text: string) => Reply | Promise<Reply>, catalog: Catalog | null = shopCatalog("c1", 1)) {
  mockIPC(async (cmd, args) => (cmd === "completion_context" ? answer((args as { text: string }).text) : null));
  seedEditor("t1", text);
  configureSql("t1", catalog ? buildCatalogView(catalog) : undefined, "Postgres");
  const view = new EditorView({ state: editorStateOf("t1"), parent: document.body });
  const type = (typed: string) => {
    for (const ch of typed)
      view.dispatch({ changes: { from: view.state.doc.length, insert: ch }, selection: { anchor: view.state.doc.length + 1 }, userEvent: "input.type" });
  };
  return { view, type };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 150));

const AFTER_TABLE = [
  "JOIN",
  "LEFT JOIN",
  "INNER JOIN",
  "RIGHT JOIN",
  "FULL JOIN",
  "CROSS JOIN",
  "WHERE",
  "GROUP BY",
  "HAVING",
  "ORDER BY",
  "LIMIT",
  "OFFSET",
  "UNION",
  "INTERSECT",
  "EXCEPT",
];

it.each(["ord", "li", "le", "gr", "ri", "fu", "of", "in", "cr", "ha", "ex", "un", "jo", "wh"])(
  "keeps the alias %s on Enter after a table and still lists the keywords",
  async (alias) => {
    const base = "select * from orders ";
    const { view, type } = mountWith(base, (text) =>
      reply({ kind: "Alias" }, [table("orders", "orders")], { keywords: AFTER_TABLE, from: text.length - (text.length - base.length) }),
    );
    try {
      for (const ch of alias) {
        type(ch);
        await settle();
      }
      const labels = currentCompletions(view.state).map((c) => c.label);
      expect(labels[0]).toBe(alias);
      expect(labels.length).toBeGreaterThan(1);
      acceptCompletion(view);
      expect(view.state.doc.toString()).toBe(base + alias);
    } finally {
      view.destroy();
    }
  },
);

it("picks a keyword after a table once the arrow keys move to it", async () => {
  const base = "select * from orders ";
  const { view, type } = mountWith(base, () => reply({ kind: "Alias" }, [table("orders", "orders")], { keywords: AFTER_TABLE, from: base.length }));
  try {
    type("wh");
    await settle();
    moveCompletionSelection(true)(view);
    acceptCompletion(view);
    expect(view.state.doc.toString()).toBe(base + "WHERE");
  } finally {
    view.destroy();
  }
});

it("settles a reply with no options that came back after more typing", async () => {
  const asked: string[] = [];
  const pending: (() => void)[] = [];
  const { view, type } = mountWith(
    "select * from ",
    async (text) => {
      asked.push(text);
      await new Promise<void>((resolve) => pending.push(resolve));
      return reply({ kind: "Relation" }, [], { from: 14 });
    },
    null,
  );
  try {
    type("x");
    await settle();
    type("y");
    pending.shift()!();
    await settle();
    expect(asked).toEqual(["select * from x"]);
    expect(completionStatus(view.state)).toBeNull();
  } finally {
    view.destroy();
  }
});

it("asks after a letter outside ASCII, as the engine reads it", async () => {
  const { sent } = await completeAtEnd("select * from caf\u00e9", reply({ kind: "Relation" }), { explicit: false });
  expect(sent).toHaveLength(1);
});

it("sends no request for a waiting completion that CodeMirror already cancelled", async () => {
  const sent: string[] = [];
  const pending: (() => void)[] = [];
  mockIPC(async (cmd, args) => {
    if (cmd !== "completion_context") return null;
    sent.push((args as { text: string }).text);
    await new Promise<void>((resolve) => pending.push(resolve));
    return reply({ kind: "Keyword" }, [], { keywords: ["SELECT"], from: 0 });
  });
  seedEditor("t1", "");
  configureSql("t1", buildCatalogView(shopCatalog("c1", 1)));
  const base = editorStateOf("t1");
  const [source] = base.languageDataAt<(c: CompletionContext) => Promise<CompletionResult | null>>("autocomplete", 0);
  const context = (text: string) => new CompletionContext(base.update({ changes: { from: 0, insert: text } }).state, text.length, true);
  const first = source!(context("s"));
  const cancelled = context("se");
  const second = source!(cancelled);
  (cancelled as unknown as { abortListeners: null }).abortListeners = null;
  await new Promise((resolve) => setTimeout(resolve, 0));
  pending.shift()!();
  await first;
  expect(await second).toBeNull();
  expect(sent).toEqual(["s"]);
});

it("waits for a pause in typing before it sends a large document", async () => {
  const asked: string[] = [];
  const base = `select 1;\n${"-- filler line\n".repeat(80_000)}select * from orders where `;
  const { view, type } = mountWith(base, (text) => {
    asked.push(text.slice(base.length));
    return reply({ kind: "Expression" }, [table("orders", "orders")], { from: base.length });
  });
  try {
    type("s");
    await new Promise((resolve) => setTimeout(resolve, 50));
    type("t");
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(asked).toEqual(["st"]);
    expect(currentCompletions(view.state).map((c) => c.label)).toContain("status");
  } finally {
    view.destroy();
  }
});

it.each([
  ["Postgres", "lab", '"a(b"', '"a(b"()'],
  ["SqlServer", "a(b", "f", "[a(b].[f](NULL)"],
] as const)("puts the cursor in a %s call's argument list, past a ( in its name", async (kind, schema, quoted, insertText) => {
  const catalog = shopCatalog("c1", 1);
  catalog.databaseKind = kind;
  const lab = catalog.schemas[0]!;
  Object.assign(lab, { name: schema, quoted: kind === "Postgres" ? schema : `[${schema}]` });
  lab.functions = [{ oid: 9, name: kind === "Postgres" ? "a(b" : "f", quoted, insertText, args: "x int", returns: "int", kind: "Function" }];
  const options = await completionOptions(buildCatalogView(catalog), reply({ kind: "Expression" }, [], { from: 7 }));
  const call = options.find((o) => o.type === "function" && typeof o.apply === "function" && String(o.label).startsWith(schema))!;
  const view = new EditorView({ doc: "select ", parent: document.body });
  try {
    (call.apply as (v: EditorView, c: typeof call, from: number, to: number) => void)(view, call, 7, 7);
    const text = view.state.doc.toString();
    const head = view.state.selection.main.head;
    expect(text.slice(head - 1, head + 1)).toBe(kind === "Postgres" ? "()" : "(N");
    expect(text.lastIndexOf("(") + 1).toBe(head);
  } finally {
    view.destroy();
  }
});

it("keeps one completion request in flight and sends only the newest one that waited", async () => {
  const sent: string[] = [];
  const pending: (() => void)[] = [];
  mockIPC(async (cmd, args) => {
    if (cmd !== "completion_context") return null;
    sent.push((args as { text: string }).text);
    await new Promise<void>((resolve) => pending.push(resolve));
    return reply({ kind: "Keyword" }, [], { keywords: ["SELECT"], from: 0 });
  });
  seedEditor("t1", "");
  configureSql("t1", buildCatalogView(shopCatalog("c1", 1)));
  const base = editorStateOf("t1");
  const [source] = base.languageDataAt<(c: CompletionContext) => Promise<CompletionResult | null>>("autocomplete", 0);
  const ask = (text: string) => source!(new CompletionContext(base.update({ changes: { from: 0, insert: text } }).state, text.length, true));
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
  const [first, second, third] = [ask("s"), ask("se"), ask("sel")];
  await settle();
  await settle();
  expect(sent).toEqual(["s"]);
  expect(await second).toBeNull();
  pending.shift()!();
  expect((await first)?.options.map((o) => o.label)).toEqual(["SELECT"]);
  await settle();
  await settle();
  expect(sent).toEqual(["s", "sel"]);
  pending.shift()!();
  expect((await third)?.options.map((o) => o.label)).toEqual(["SELECT"]);
});

it("drops the join conditions when the relation details take longer than 250 ms", async () => {
  vi.useFakeTimers();
  try {
    mockIPC((cmd) => (cmd === "relation_detail" ? new Promise(() => {}) : null));
    const options = completionOptions(
      buildCatalogView(shopCatalog("c1", 1)),
      reply({ kind: "JoinCondition", target: 1 }, [table("o", "orders"), table("c", "customers")]),
    );
    await vi.advanceTimersByTimeAsync(249);
    let settled = false;
    void options.then(() => (settled = true));
    await Promise.resolve();
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    const labels = (await options).map((o) => o.label);
    expect(labels).not.toContain("c.id = o.customer_id");
    expect(labels).toContain("email");
  } finally {
    vi.useRealTimers();
  }
});

it("changes to SQL Server syntax before a catalog arrives while retaining the document", () => {
  seedEditor("t1", "SELECT @name FROM [line items]");
  configureSql("t1", undefined, "SqlServer");
  const state = editorStateOf("t1");
  const names: string[] = [];
  syntaxTree(state).iterate({
    enter: (node) => {
      if (node.from === 18) names.push(node.name);
    },
  });
  expect(names).toContain("QuotedIdentifier");
  expect(state.doc.toString()).toBe("SELECT @name FROM [line items]");
  configureSql("t1", undefined, "Postgres");
  expect(syntaxTree(editorStateOf("t1")).toString()).not.toContain("QuotedIdentifier");
});

it("tells listeners when a background tab's text changes, and not for a cursor move or a flash", () => {
  seedEditor("t1", "select 1;");
  let calls = 0;
  const stop = onBuffersChanged(() => calls++);
  moveCursor("t1", 3);
  flashRanges("t1", [{ from: 0, to: 6 }]);
  expect(calls).toBe(0);
  replaceDoc("t1", "select 2;");
  expect(calls).toBe(1);
  stop();
});

it("forgets a dropped tab's doc", () => {
  seedEditor("t1", "select 1;");
  dropEditor("t1");
  expect(docOf("t1")).toBeUndefined();
});
