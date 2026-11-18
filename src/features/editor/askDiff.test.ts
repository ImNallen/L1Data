import { EditorState } from "@codemirror/state";
import { expect, it } from "vitest";
import { askPreviewEffect, askPreviewField, changedHunks } from "./askDiff";

it("shows separate changed hunks without repeating unchanged lines", () => {
  const doc = EditorState.create({ doc: "SELECT a,\n  b,\n  c\nFROM orders\nWHERE paid\nORDER BY a" }).doc;
  const sql = "SELECT a,\n  total,\n  c\nFROM orders\nWHERE shipped\nORDER BY a";
  const hunks = changedHunks(doc, { from: 0, to: doc.length }, sql);
  expect(hunks.map((h) => ({ removed: h.removed.map((at) => doc.lineAt(at).text), added: h.added }))).toEqual([
    { removed: ["  b,"], added: ["  total,"] },
    { removed: ["WHERE paid"], added: ["WHERE shipped"] },
  ]);
});

it("keeps partial-line context around the captured range in its preview", () => {
  const doc = EditorState.create({ doc: "SELECT total FROM orders" }).doc;
  expect(changedHunks(doc, { from: 7, to: 12 }, "sum(total)")).toEqual([
    { from: 0, to: doc.length, removed: [0], added: ["SELECT sum(total) FROM orders"], side: 1 },
  ]);
});

it("handles empty insertion and deletion without inventing blank additions", () => {
  const empty = EditorState.create().doc;
  expect(changedHunks(empty, { from: 0, to: 0 }, "SELECT 1;")[0]?.added).toEqual(["SELECT 1;"]);
  const full = EditorState.create({ doc: "SELECT 1;" }).doc;
  expect(changedHunks(full, { from: 0, to: full.length }, "")[0]?.added).toEqual([]);
  expect(changedHunks(full, { from: 0, to: full.length }, "SELECT 1;")).toEqual([]);
});

it("adds between unchanged lines at the next line start and after the final line at the end", () => {
  const doc = EditorState.create({ doc: "a\nc" }).doc;
  expect(changedHunks(doc, { from: 0, to: doc.length }, "a\nb\nc")).toEqual([{ from: 2, to: 2, removed: [], added: ["b"], side: -1 }]);
  expect(changedHunks(doc, { from: 0, to: doc.length }, "a\nc\nd")).toEqual([{ from: 3, to: 3, removed: [], added: ["d"], side: 1 }]);
});

it("installs a preview over the original immutable document and removes it on edit", () => {
  const state = EditorState.create({ doc: "SELECT 1;", extensions: [askPreviewField] });
  const preview = state.update({
    effects: askPreviewEffect.of({ doc: state.doc, range: { from: 0, to: 9 }, sql: "SELECT 2;" }),
  }).state;
  expect(preview.doc).toBe(state.doc);
  expect(preview.field(askPreviewField).size).toBe(2);
  expect(preview.update({ changes: { from: 0, insert: " " } }).state.field(askPreviewField).size).toBe(0);
});
