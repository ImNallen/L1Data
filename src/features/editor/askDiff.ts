import { StateEffect, StateField, type EditorState, type Text } from "@codemirror/state";
import { Decoration, EditorView, WidgetType, type DecorationSet } from "@codemirror/view";
import type { DocRange } from "../../ipc/bindings";

export type ChangedHunk = { from: number; to: number; removed: number[]; added: string[]; side: -1 | 1 };
export type AskPreview = { doc: Text; range: DocRange; sql: string };
const MAX_COMPARISON_CELLS = 1_000_000;

/** Compare complete affected lines, including unchanged text around a partial-line selection. */
export function changedHunks(doc: Text, range: DocRange, sql: string): ChangedHunk[] {
  const first = doc.lineAt(range.from);
  const last = doc.lineAt(range.to);
  const original = doc.sliceString(first.from, last.to);
  const candidate = doc.sliceString(first.from, range.from) + sql + doc.sliceString(range.to, last.to);
  const before = original ? original.split("\n") : [];
  const after = candidate ? candidate.split("\n") : [];
  let prefix = 0;
  while (prefix < before.length && prefix < after.length && before[prefix] === after[prefix]) prefix++;
  let oldEnd = before.length;
  let newEnd = after.length;
  while (oldEnd > prefix && newEnd > prefix && before[oldEnd - 1] === after[newEnd - 1]) {
    oldEnd--;
    newEnd--;
  }
  if (oldEnd === prefix && newEnd === prefix) return [];
  const offsets = before.map((_, index) => doc.line(first.number + index).from);
  const anchor = (index: number) => offsets[index] ?? last.to;
  const hunk = (start: number, end: number, added: string[]): ChangedHunk => ({
    from: anchor(start),
    to: end > start ? doc.line(first.number + end - 1).to : anchor(start),
    removed: offsets.slice(start, end),
    added,
    side: end > start || start === before.length ? 1 : -1,
  });
  const oldCount = oldEnd - prefix;
  const newCount = newEnd - prefix;
  if (oldCount * newCount > MAX_COMPARISON_CELLS) return [hunk(prefix, oldEnd, after.slice(prefix, newEnd))];
  const width = newCount + 1;
  const lengths = new Uint32Array((oldCount + 1) * width);
  for (let i = oldCount - 1; i >= 0; i--) {
    for (let j = newCount - 1; j >= 0; j--) {
      lengths[i * width + j] =
        before[prefix + i] === after[prefix + j]
          ? 1 + (lengths[(i + 1) * width + j + 1] ?? 0)
          : Math.max(lengths[(i + 1) * width + j] ?? 0, lengths[i * width + j + 1] ?? 0);
    }
  }
  const hunks: ChangedHunk[] = [];
  let i = 0;
  let j = 0;
  while (i < oldCount || j < newCount) {
    if (i < oldCount && j < newCount && before[prefix + i] === after[prefix + j]) {
      i++;
      j++;
      continue;
    }
    const start = i;
    const added: string[] = [];
    while (i < oldCount || j < newCount) {
      if (i < oldCount && j < newCount && before[prefix + i] === after[prefix + j]) break;
      if (j < newCount && (i === oldCount || (lengths[i * width + j + 1] ?? 0) > (lengths[(i + 1) * width + j] ?? 0))) {
        added.push(after[prefix + j] ?? "");
        j++;
      } else i++;
    }
    hunks.push(hunk(prefix + start, prefix + i, added));
  }
  return hunks;
}

class AddedLines extends WidgetType {
  constructor(readonly lines: string[]) {
    super();
  }
  eq(other: AddedLines): boolean {
    return this.lines.join("\n") === other.lines.join("\n");
  }
  toDOM(): HTMLElement {
    const block = document.createElement("div");
    block.className = "cm-ask-added";
    block.setAttribute("aria-label", "Proposed SQL additions");
    for (const line of this.lines) {
      const row = document.createElement("div");
      row.className = "cm-ask-added-line";
      const marker = document.createElement("span");
      marker.textContent = "+";
      marker.className = "cm-ask-marker";
      row.append(marker);
      row.append(document.createTextNode(line));
      block.append(row);
    }
    return block;
  }
  ignoreEvent(): boolean {
    return false;
  }
}

export const askPreviewEffect = StateEffect.define<AskPreview | null>();
export const askPreviewField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(decorations, transaction) {
    let next = transaction.docChanged ? Decoration.none : decorations;
    for (const effect of transaction.effects) {
      if (!effect.is(askPreviewEffect)) continue;
      next = effect.value && effect.value.doc === transaction.state.doc ? decorationsFor(transaction.state, effect.value) : Decoration.none;
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

function decorationsFor(state: EditorState, preview: AskPreview): DecorationSet {
  return Decoration.set(
    changedHunks(state.doc, preview.range, preview.sql).flatMap((hunk) => [
      ...hunk.removed.map((at) => Decoration.line({ class: "cm-ask-removed" }).range(at)),
      ...(hunk.added.length
        ? [
            Decoration.widget({
              widget: new AddedLines(hunk.added),
              block: true,
              side: hunk.side,
            }).range(hunk.to),
          ]
        : []),
    ]),
    true,
  );
}

export const askDiffTheme = EditorView.theme({
  ".cm-ask-removed": { backgroundColor: "color-mix(in srgb, var(--color-danger) 11%, transparent)", position: "relative" },
  ".cm-ask-removed, .cm-ask-removed span": { color: "var(--color-danger) !important" },
  ".cm-ask-removed::before": { content: "'-'", position: "absolute", left: "4px", color: "var(--color-danger)" },
  ".cm-ask-added": {
    maxHeight: "240px",
    overflow: "auto",
    color: "var(--color-green)",
    backgroundColor: "color-mix(in srgb, var(--color-green) 12%, transparent)",
  },
  ".cm-ask-added-line": { minHeight: "1lh", padding: "0 16px 0 18px", whiteSpace: "pre", position: "relative" },
  ".cm-ask-marker": { position: "absolute", left: "4px", color: "var(--color-green)", userSelect: "none" },
});
