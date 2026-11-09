import type { Completion, CompletionContext as EditorContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { EditorView } from "@codemirror/view";
import { commands, type CompletionContext, type DatabaseKind, type Relation, type ScopeRelation } from "../../ipc/bindings";
import { isViewKind, type CatalogView } from "../../state/catalog";
import { detailKey, useRelationDetail } from "../../state/relationDetail";
import { shortType } from "../results/format";
import { docText } from "./docText";

type Complete = Extract<CompletionContext, { kind: "Complete" }>;
type Resolved = { schema: string; rel: Relation };
type Column = { name: string; insert: string; detail: string };

const BOOST = { typed: 100, join: 99, column: 4, reference: 3, relation: 2, function: 1, keyword: -1 };

/** The engine's word characters, so a request follows the same words it reads, such as `café` or `@id`. */
const WORD = /^[\p{L}\p{N}_$#@]*$/u;
const TYPED = /[\p{L}\p{N}_$#@]+/u;
const ASKS_AFTER = /[\p{L}\p{N}_$#@.\s{]/u;
const REFERENCE_NAME = /^[^}\n]*$/;

let cellNames: () => string[] = () => [];

/** The titles a `{{name}}` in the active cell can reference: the other cells of its notebook. */
export function provideCellNames(names: () => string[]): void {
  cellNames = names;
}

function referenceResult(ctx: EditorContext, from: number): CompletionResult | null {
  const closed = ctx.state.sliceDoc(ctx.pos, ctx.pos + 2) === "}}";
  const options = [...new Set(cellNames())].map((name): Completion => ({
    label: name,
    type: "variable",
    detail: "cell",
    apply: (v, _c, start, end) => {
      const insert = closed ? name : `${name}}}`;
      v.dispatch({ changes: { from: start, to: end, insert }, selection: { anchor: start + insert.length + (closed ? 2 : 0) } });
    },
  }));
  return options.length ? { from, options, validFor: REFERENCE_NAME } : null;
}

const JOIN_DETAIL_WAIT_MS = 250;

/**
 * Each request copies the whole document across IPC, which on a document this large costs tens of milliseconds of the
 * main thread. So past this size a request asked while typing waits for a pause, as CodeMirror's default typing delay did.
 */
const LARGE_DOC_CHARS = 1_000_000;
const LARGE_DOC_PAUSE_MS = 150;

/**
 * A call's text ends with its argument list, and the engine's argument templates hold no parentheses, so the list opens at
 * the last `(`. The first one could sit inside a quoted name such as `"a(b"`.
 */
function insertCallWithCursorInside(text: string) {
  return (view: EditorView, _c: Completion, from: number, to: number) => {
    const open = text.lastIndexOf("(");
    const anchor = open >= 0 && text.endsWith(")") ? from + open + 1 : from + text.length;
    view.dispatch({ changes: { from, to, insert: text }, selection: { anchor }, userEvent: "input.complete" });
  };
}

function serverMatch<T extends { name: string }>(view: CatalogView, items: T[], name: string): T | undefined {
  const exact = items.find((i) => i.name === name);
  if (exact || view.catalog.databaseKind !== "SqlServer") return exact;
  return items.find((i) => i.name.toLowerCase() === name.toLowerCase());
}

function schemaNamed(view: CatalogView, name: string) {
  return serverMatch(view, view.catalog.schemas, name);
}

function resolveTable(view: CatalogView | undefined, schema: string | null, name: string): Resolved | undefined {
  if (!view) return undefined;
  const target = schema ?? view.catalog.defaultSchema;
  const s = target === null ? undefined : schemaNamed(view, target);
  const rel = s && serverMatch(view, s.relations, name);
  return s && rel ? { schema: s.name, rel } : undefined;
}

function renamed(columns: Column[], aliases: string[], detail: string): Column[] {
  return [...aliases.map((name, i) => ({ name, insert: name, detail: columns[i]?.detail ?? detail })), ...columns.slice(aliases.length)];
}

function catalogColumns(found: Resolved | undefined, from: string): Column[] {
  return found?.rel.columns.map((c) => ({ name: c.name, insert: c.quoted, detail: `${shortType(c.typeDisplay)} · ${from}` })) ?? [];
}

function columnsOf(view: CatalogView | undefined, rel: ScopeRelation): Column[] {
  const from = rel.reference ?? "";
  if (rel.source.kind === "Table") return renamed(catalogColumns(resolveTable(view, rel.source.schema, rel.source.name), from), rel.columnAliases, from);
  const named = rel.source.columns.map((name) => ({ name, insert: name, detail: from }));
  const starred = rel.source.star.flatMap((t) => catalogColumns(resolveTable(view, t.schema, t.name), from));
  return renamed([...named, ...starred], rel.columnAliases, from);
}

function columnOptions(view: CatalogView | undefined, rel: ScopeRelation): Completion[] {
  return columnsOf(view, rel).map((c) => ({ label: c.name, apply: c.insert, type: "property", detail: c.detail, boost: BOOST.column }));
}

function relationOption(rel: Relation, label = rel.name, apply = rel.quoted): Completion {
  const view = isViewKind(rel.kind);
  return { label, apply, type: view ? "type" : "class", detail: view ? "view" : "table", boost: BOOST.relation };
}

function keywordOptions(reply: Complete): Completion[] {
  return reply.keywords.map((k) => ({ label: k, type: "keyword", boost: BOOST.keyword }));
}

/** Keys load per relation in scope, so a catalog refresh never reads every key in the database. */
async function joinConditions(view: CatalogView, reply: Complete, target: number): Promise<Completion[]> {
  const resolve = (r: ScopeRelation | undefined) => {
    const found = r?.source.kind === "Table" && r.reference ? resolveTable(view, r.source.schema, r.source.name) : undefined;
    return found && r?.reference ? [{ ...found, ref: r.reference, aliases: r.columnAliases }] : [];
  };
  const [t] = resolve(reply.relations[target]);
  if (!t) return [];
  const others = reply.relations.flatMap((r, i) => (i === target ? [] : resolve(r)));
  if (!others.length) return [];
  const { db, version } = view.catalog;
  const details = useRelationDetail.getState();
  const loaded = Promise.all([t, ...others].map((r) => details.ensure(db, version, r.rel.oid)));
  await Promise.race([loaded, new Promise((resolve) => setTimeout(resolve, JOIN_DETAIL_WAIT_MS))]);
  const keysOf = (oid: number) => useRelationDetail.getState().entries[detailKey(db, oid)]?.detail?.keys ?? [];
  const col = (r: (typeof others)[number], name: string) => {
    const i = r.rel.columns.findIndex((c) => c.name === name);
    const quoted = r.aliases[i] ?? r.rel.columns[i]?.quoted;
    return quoted === undefined ? undefined : `${r.ref}.${quoted}`;
  };
  const condition = (pairs: [string | undefined, string | undefined][]) =>
    pairs.every(([a, b]) => a !== undefined && b !== undefined) ? pairs.map(([a, b]) => `${a} = ${b}`).join(" AND ") : undefined;
  const conditions: (string | undefined)[] = [];
  for (const o of others) {
    for (const key of keysOf(t.rel.oid)) {
      if (key.kind.kind === "Foreign" && key.kind.refSchema === o.schema && key.kind.refTable === o.rel.name) {
        const refs = key.kind.refColumns;
        conditions.push(condition(key.columns.map((c, i) => [col(t, c), col(o, refs[i]!)])));
      }
    }
    for (const key of keysOf(o.rel.oid)) {
      if (key.kind.kind === "Foreign" && key.kind.refSchema === t.schema && key.kind.refTable === t.rel.name) {
        const refs = key.kind.refColumns;
        conditions.push(condition(key.columns.map((c, i) => [col(t, refs[i]!), col(o, c)])));
      }
    }
  }
  return [...new Set(conditions.filter((c) => c !== undefined))].map((label) => ({ label, type: "text", detail: "foreign key", boost: BOOST.join }));
}

function qualifiedOptions(view: CatalogView | undefined, reply: Complete): Completion[] {
  const q = reply.qualifier;
  if (!q) return [];
  if (q.kind === "Relation") {
    const rel = reply.relations[q.index];
    return rel ? columnOptions(view, rel) : [];
  }
  if (!view) return [];
  const [first, second] = q.parts;
  if (q.parts.length === 2 && first !== undefined && second !== undefined) {
    const found = resolveTable(view, first, second);
    return found ? found.rel.columns.map((c) => ({ label: c.name, apply: c.quoted, type: "property", detail: shortType(c.typeDisplay) })) : [];
  }
  const schema = q.parts.length === 1 && first !== undefined ? schemaNamed(view, first) : undefined;
  if (!schema) return [];
  const relations = schema.relations.map((r) => relationOption(r));
  if (reply.expect.kind === "Relation") return relations;
  const functions = schema.functions
    .filter((f) => f.kind !== "Procedure")
    .map((f): Completion => ({
      label: f.name,
      detail: `(${f.args})`,
      type: "function",
      apply: insertCallWithCursorInside(`${f.quoted}()`),
      boost: BOOST.function,
    }));
  return [...relations, ...functions];
}

/** Two or more letters that begin a keyword that can follow, such as `wh` of `WHERE`. */
function startsKeyword(typed: string, keywords: string[]): boolean {
  const upper = typed.toUpperCase();
  return typed.length >= 2 && keywords.some((k) => k.startsWith(upper));
}

/** Where an alias may stand, the word as typed comes first, so Enter keeps it and a keyword takes an arrow key to pick. */
function aliasOptions(reply: Complete, typed: string): Completion[] {
  const keep: Completion[] = typed ? [{ label: typed, type: "variable", detail: "alias", boost: BOOST.typed }] : [];
  return [...keep, ...keywordOptions(reply)];
}

export async function completionOptions(view: CatalogView | undefined, reply: Complete, typed = ""): Promise<Completion[]> {
  const joins = reply.expect.kind === "JoinCondition" && !reply.qualifier && view ? await joinConditions(view, reply, reply.expect.target) : [];
  return [...joins, ...optionsFor(view, reply, typed)];
}

/** The options a reply gives, apart from join conditions, which wait on relation details. */
function optionsFor(view: CatalogView | undefined, reply: Complete, typed: string): Completion[] {
  if (reply.qualifier) return qualifiedOptions(view, reply);
  const expect = reply.expect;
  if (expect.kind === "Alias") return aliasOptions(reply, typed);
  if (expect.kind === "Keyword") return keywordOptions(reply);
  if (expect.kind === "Relation") {
    const schema = view?.catalog.defaultSchema;
    const local = view?.catalog.schemas.find((s) => s.name === schema)?.relations.map((r) => relationOption(r)) ?? [];
    const ctes = reply.ctes.map((name): Completion => ({ label: name, type: "type", detail: "CTE", boost: BOOST.relation }));
    return [...ctes, ...local, ...(view?.qualifiedTables ?? []), ...keywordOptions(reply)];
  }
  const columns = reply.relations.flatMap((r) => columnOptions(view, r));
  const references = reply.relations.flatMap((r): Completion[] => (r.reference ? [{ label: r.reference, type: "variable", boost: BOOST.reference }] : []));
  const catalogFunctions = (view?.functions ?? []).map((f) => ({
    ...f,
    apply: typeof f.apply === "string" ? insertCallWithCursorInside(f.apply) : f.apply,
    boost: BOOST.function,
  }));
  const builtins = reply.functions.map((name): Completion => ({ label: name, type: "function", apply: insertCallWithCursorInside(`${name}()`) }));
  return [...columns, ...references, ...catalogFunctions, ...builtins, ...keywordOptions(reply)];
}

function opensUnasked(ctx: EditorContext, reply: Complete, typed: string): boolean {
  if (ctx.explicit) return true;
  if (reply.expect.kind === "Alias") return startsKeyword(typed, reply.keywords);
  return typed !== "" || ctx.state.sliceDoc(ctx.pos - 1, ctx.pos) === "." || reply.expect.kind === "JoinCondition";
}

/** The next task, so a request's copy of a large document never shares a task with the keystroke that asked for it. */
const nextTask = () => new Promise((resolve) => setTimeout(resolve, 0));

function inFlightThenNewest<T>(send: (ctx: EditorContext) => Promise<T>): (ctx: EditorContext) => Promise<T | null> {
  let busy = false;
  let waiting: { ctx: EditorContext; resolve: (reply: Promise<T | null>) => void } | null = null;
  const start = (ctx: EditorContext): Promise<T> => {
    busy = true;
    const reply = send(ctx);
    void reply
      .catch(() => {})
      .finally(() => {
        busy = false;
        const next = waiting;
        waiting = null;
        next?.resolve(next.ctx.aborted ? Promise.resolve(null) : start(next.ctx));
      });
    return reply;
  };
  return (ctx) => {
    if (!busy) return start(ctx);
    waiting?.resolve(Promise.resolve(null));
    return new Promise((resolve) => (waiting = { ctx, resolve }));
  };
}

/**
 * Declines to open without returning null. When a request returns null, CodeMirror never asks again for a word typed
 * while it was in flight, so typing `wh` quickly after a table would never offer `WHERE`.
 */
function declined(from: number, validFor: (text: string) => boolean = () => false, update?: CompletionResult["update"]): CompletionResult {
  return { from, options: [], validFor, update };
}

/**
 * A reply holds for any word typed at its `from`, since the engine skips the word being typed. So more typing in that
 * word is answered from the same reply: a declined reply can still open, and an alias list keeps the word as typed first.
 */
function retype(view: CatalogView | undefined, reply: Complete): CompletionResult["update"] {
  return (_current, from, to, ctx) => {
    const typed = ctx.state.sliceDoc(from, to);
    return typed && WORD.test(typed) ? resultFor(view, reply, from, typed, ctx) : null;
  };
}

function resultFor(
  view: CatalogView | undefined,
  reply: Complete,
  from: number,
  typed: string,
  ctx: EditorContext,
  joins: Completion[] = [],
): CompletionResult {
  if (!opensUnasked(ctx, reply, typed)) return declined(from, () => false, retype(view, reply));
  if (reply.expect.kind === "Alias" && !reply.qualifier)
    return { from, options: aliasOptions(reply, typed), validFor: (t) => t === typed, update: retype(view, reply) };
  const options = [...joins, ...optionsFor(view, reply, typed)];
  return options.length ? { from, options, validFor: WORD } : declined(from, () => false, retype(view, reply));
}

export function engineCompletion(view: CatalogView | undefined, kind: DatabaseKind): CompletionSource {
  const ask = inFlightThenNewest(async (ctx) => {
    await nextTask();
    return ctx.aborted ? null : commands.completionContext(docText(ctx.state.doc), ctx.pos, kind);
  });
  return async (ctx: EditorContext): Promise<CompletionResult | null> => {
    const before = ctx.state.sliceDoc(ctx.pos - 1, ctx.pos);
    if (!ctx.explicit && !ASKS_AFTER.test(before)) return null;
    if (!ctx.explicit && ctx.state.doc.length > LARGE_DOC_CHARS) {
      await new Promise((resolve) => setTimeout(resolve, LARGE_DOC_PAUSE_MS));
      if (ctx.aborted || (ctx.view && ctx.view.state.doc !== ctx.state.doc)) return declined(ctx.pos);
    }
    const reply = await ask(ctx);
    if (!reply || ctx.aborted) return null;
    if (reply.kind === "None") return declined(ctx.matchBefore(TYPED)?.from ?? ctx.pos, (text) => WORD.test(text));
    if (reply.kind === "Reference") return referenceResult(ctx, reply.from);
    const typed = ctx.state.sliceDoc(reply.from, ctx.pos);
    const joining = reply.expect.kind === "JoinCondition" && !reply.qualifier && view && opensUnasked(ctx, reply, typed);
    const joins = joining && reply.expect.kind === "JoinCondition" ? await joinConditions(view, reply, reply.expect.target) : [];
    if (ctx.aborted) return null;
    return resultFor(view, reply, reply.from, typed, ctx, joins);
  };
}
