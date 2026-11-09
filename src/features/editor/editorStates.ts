import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab, isolateHistory } from "@codemirror/commands";
import { bracketMatching, indentOnInput } from "@codemirror/language";
import { setDiagnostics, type Diagnostic } from "@codemirror/lint";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { Compartment, countColumn, EditorState, StateEffect, StateField, type Extension, type Text, type TransactionSpec } from "@codemirror/state";
import {
  Decoration,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  layer,
  lineNumbers,
  placeholder,
  type DecorationSet,
  type LayerMarker,
} from "@codemirror/view";
import type { DatabaseKind, DocRange, FormatEdit, TabId } from "../../ipc/bindings";
import type { CatalogView } from "../../state/catalog";
import { engineCompletion } from "./completion";
import { SQL_DIALECT } from "./sqlDialect";
import { editorHighlighting, editorTheme, functionCallStyle, LINE_PADDING_LEFT } from "./theme";

const states = new Map<TabId, EditorState>();
const bufferListeners = new Set<() => void>();
const contextListeners = new Set<() => void>();
let view: EditorView | null = null;
let activeId: TabId | null = null;
let behavior: Extension = [];

const sqlConfig = new Compartment();

const flashEffect = StateEffect.define<DocRange[]>();
const clearFlash = StateEffect.define<null>();
const flashMark = Decoration.mark({ class: "cm-executed" });

const flashField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    let next = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(flashEffect)) {
        const len = tr.state.doc.length;
        const ranges = e.value
          .map((r) => ({ from: Math.min(r.from, len), to: Math.min(r.to, len) }))
          .filter((r) => r.to > r.from)
          .map((r) => flashMark.range(r.from, r.to));
        next = Decoration.set(ranges, true);
      }
      if (e.is(clearFlash)) next = Decoration.none;
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const mapEffect = StateEffect.define<DocRange[]>();
const mapMark = Decoration.mark({ class: "cm-map-source" });
const mapField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    let next = tr.docChanged ? Decoration.none : deco;
    for (const effect of tr.effects) {
      if (effect.is(mapEffect)) {
        next = Decoration.set(
          effect.value.filter((r) => r.from < r.to && r.from >= 0 && r.to <= tr.state.doc.length).map((r) => mapMark.range(r.from, r.to)),
          true,
        );
      }
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export type ReferenceMark = { from: number; to: number; name: string; known: boolean };

const referenceEffect = StateEffect.define<{ doc: Text; marks: ReferenceMark[] }>();
// Maps through edits so marks hold still until the engine's next answer replaces them.
const referenceField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    let next = deco.map(tr.changes);
    for (const effect of tr.effects) {
      if (!effect.is(referenceEffect) || effect.value.doc !== tr.state.doc) continue;
      const len = tr.state.doc.length;
      next = Decoration.set(
        effect.value.marks
          .filter((m) => m.from < m.to && m.from >= 0 && m.to <= len)
          .map((m) =>
            Decoration.mark({
              class: m.known ? "cm-cell-ref" : "cm-cell-ref cm-cell-ref-unknown",
              attributes: { title: m.known ? `${m.name} · ⌘-click to go to the cell` : `No cell is named ${m.name}` },
              name: m.name,
            }).range(m.from, m.to),
          ),
        true,
      );
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/** Marks for the doc the engine read. A doc that changed since is left alone: its own pass is already on the way. */
export function showReferences(id: TabId, doc: Text, marks: ReferenceMark[]): void {
  updateEditor(id, () => ({ effects: referenceEffect.of({ doc, marks }) }));
}

/** The name of the reference under `pos`, if any. */
export function referenceAt(state: EditorState, pos: number): string | null {
  let name: string | null = null;
  state.field(referenceField, false)?.between(pos, pos, (from, to, deco) => {
    if (from <= pos && pos <= to) name = (deco.spec as { name?: string }).name ?? null;
  });
  return name;
}

export type EditorContext = { tabId: TabId; doc: Text; selection: EditorState["selection"] };

export function editorContextOf(tabId: TabId): EditorContext {
  const state = editorStateOf(tabId);
  return { tabId, doc: state.doc, selection: state.selection };
}

export function sameEditorContext(a: EditorContext, b: EditorContext): boolean {
  return a.tabId === b.tabId && a.doc === b.doc && a.selection.eq(b.selection);
}

export function onEditorContextChanged(listener: () => void): () => void {
  contextListeners.add(listener);
  return () => contextListeners.delete(listener);
}

export function clearMapHighlights(id: TabId): void {
  updateEditor(id, () => ({ effects: mapEffect.of([]) }));
}

export function highlightMapSource(context: EditorContext, ranges: DocRange[]): boolean {
  if (activeId !== context.tabId || !sameEditorContext(context, editorContextOf(context.tabId))) return false;
  const unique = ranges.filter((r, i) => !ranges.slice(0, i).some((p) => p.from === r.from && p.to === r.to));
  const first = unique[0];
  updateEditor(context.tabId, () => ({ effects: first ? [mapEffect.of(unique), EditorView.scrollIntoView(first.from, { y: "nearest" })] : mapEffect.of([]) }));
  return true;
}

const previewEffect = StateEffect.define<DocRange[] | null>();

// The chooser closes on any edit, so its ranges never need mapping.
const previewField = StateField.define<readonly DocRange[]>({
  create: () => [],
  update(ranges, tr) {
    let next = tr.docChanged ? [] : ranges;
    for (const e of tr.effects) if (e.is(previewEffect)) next = (e.value ?? []).filter((r) => r.from < r.to && r.to <= tr.state.doc.length);
    return next;
  },
});

const runTargetEffect = StateEffect.define<DocRange | null>();

// With a selection, ⌘↵ runs the selection, so the mark goes the moment one exists and never paints over it.
const runTargetField = StateField.define<DocRange | null>({
  create: () => null,
  update(range, tr) {
    if (!tr.state.selection.main.empty) return null;
    let next = range;
    if (range && tr.docChanged) {
      const mapped = { from: tr.changes.mapPos(range.from, 1), to: tr.changes.mapPos(range.to, -1) };
      next = mapped.from < mapped.to ? mapped : null;
    }
    for (const e of tr.effects) {
      if (e.is(runTargetEffect)) next = e.value && e.value.from < e.value.to && e.value.to <= tr.state.doc.length ? e.value : null;
    }
    return next;
  },
});

class StatementBox implements LayerMarker {
  constructor(
    readonly left: number,
    readonly top: number,
    readonly width: number,
    readonly height: number,
  ) {}
  draw(): HTMLElement {
    const el = document.createElement("div");
    el.className = "cm-run-target";
    this.update(el);
    return el;
  }
  update(el: HTMLElement): boolean {
    el.style.left = `${this.left}px`;
    el.style.top = `${this.top}px`;
    el.style.width = `${this.width}px`;
    el.style.height = `${this.height}px`;
    return true;
  }
  eq(o: StatementBox): boolean {
    return o.left === this.left && o.top === this.top && o.width === this.width && o.height === this.height;
  }
}

/**
 * One rounded box behind a statement, as wide as its longest line. Width comes from columns, not drawn lines, so it holds
 * still while lines scroll in and out of the viewport, and it stops at the scroller's right edge so a very long line cannot
 * turn the whole editor into tint.
 */
function statementBox(v: EditorView, range: DocRange): StatementBox {
  const { doc, tabSize } = v.state;
  const first = doc.lineAt(range.from);
  const last = doc.lineAt(range.to);
  const startCol = first.number === last.number ? countColumn(first.text, tabSize, range.from - first.from) : 0;
  let endCol = 0;
  for (let n = first.number; n <= last.number; n++) {
    const line = doc.line(n);
    endCol = Math.max(endCol, countColumn(line.text, tabSize, Math.min(line.to, range.to) - line.from));
  }
  const scroller = v.scrollDOM.getBoundingClientRect();
  const textLeft = v.contentDOM.getBoundingClientRect().left - scroller.left + v.scrollDOM.scrollLeft + LINE_PADDING_LEFT;
  const docTop = v.documentTop - scroller.top + v.scrollDOM.scrollTop;
  const left = textLeft + startCol * v.defaultCharacterWidth - 6;
  const right = Math.min(textLeft + endCol * v.defaultCharacterWidth + 8, v.scrollDOM.clientWidth - 8);
  const top = docTop + v.lineBlockAt(range.from).top;
  const bottom = docTop + v.lineBlockAt(range.to).bottom;
  return new StatementBox(left, top, Math.max(right - left, 0), bottom - top);
}

// While the chooser previews a row, its statements take the box's place, so only what would run is boxed.
function statementBoxes(v: EditorView): readonly LayerMarker[] {
  const preview = v.state.field(previewField);
  const target = v.state.field(runTargetField);
  return (preview.length ? preview : target ? [target] : []).map((r) => statementBox(v, r));
}

const runTargetLayer = layer({
  above: false,
  class: "cm-runTargetLayer",
  markers: statementBoxes,
  update: (u) =>
    u.geometryChanged ||
    u.startState.field(runTargetField) !== u.state.field(runTargetField) ||
    u.startState.field(previewField) !== u.state.field(previewField),
});

// The selection layer paints behind the text, so the active line's background would hide it.
const selectingClass = EditorView.editorAttributes.of((v) => (v.state.selection.main.empty ? null : { class: "cm-selecting" }));

function sqlLanguage(catalog: CatalogView | undefined, databaseKind: DatabaseKind): Extension {
  const { language } = SQL_DIALECT[databaseKind];
  return [language, language.data.of({ autocomplete: engineCompletion(catalog, databaseKind) })];
}

export function installEditorBehavior(ext: Extension): void {
  behavior = ext;
}

function createState(text: string): EditorState {
  return EditorState.create({
    doc: text,
    extensions: [
      behavior,
      lineNumbers(),
      placeholder("Write SQL, or ask the assistant to add a query…"),
      highlightActiveLineGutter(),
      highlightActiveLine(),
      history(),
      drawSelection(),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      autocompletion({ icons: false, activateOnTypingDelay: 0 }),
      highlightSelectionMatches(),
      keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...completionKeymap, indentWithTab]),
      sqlConfig.of(sqlLanguage(undefined, "Postgres")),
      flashField,
      previewField,
      mapField,
      referenceField,
      runTargetField,
      runTargetLayer,
      selectingClass,
      EditorView.contentAttributes.of({ "aria-label": "SQL editor" }),
      editorTheme,
      editorHighlighting,
      functionCallStyle,
      EditorState.tabSize.of(2),
    ],
  });
}

const emit = () => {
  for (const l of bufferListeners) l();
};

/**
 * Every write to `states` goes through here, so a doc that changes in a background tab is seen too. The view's
 * update listener sees only the active tab. A first state is no change: `editorStateOf` may create one mid-render.
 */
function commit(id: TabId, next: EditorState): void {
  const prev = states.get(id);
  states.set(id, next);
  if (prev && prev.doc !== next.doc) emit();
  if (prev && (prev.doc !== next.doc || !prev.selection.eq(next.selection))) for (const listener of contextListeners) listener();
}

export function seedEditor(id: TabId, text: string): void {
  commit(id, createState(text));
}

export function editorStateOf(id: TabId): EditorState {
  if (id === activeId && view) return view.state;
  let state = states.get(id);
  if (!state) {
    state = createState("");
    commit(id, state);
  }
  return state;
}

/** The tab's doc, or `undefined` once the tab is gone. Unlike `editorStateOf`, it never creates a state. */
export function docOf(id: TabId): Text | undefined {
  return id === activeId && view ? view.state.doc : states.get(id)?.doc;
}

export function onBuffersChanged(listener: () => void): () => void {
  bufferListeners.add(listener);
  return () => bufferListeners.delete(listener);
}

export function cursorOf(state: EditorState): { line: number; col: number } {
  const head = state.selection.main.head;
  const line = state.doc.lineAt(head);
  return { line: line.number, col: head - line.from + 1 };
}

export function textOf(id: TabId): string {
  return editorStateOf(id).doc.toString();
}

export function dropEditor(id: TabId): void {
  states.delete(id);
  if (activeId === id) activeId = null;
  for (const listener of contextListeners) listener();
}

function updateEditor(id: TabId, spec: (state: EditorState) => TransactionSpec): void {
  if (id === activeId && view) {
    view.dispatch(spec(view.state));
    return;
  }
  const state = states.get(id);
  if (state) commit(id, state.update(spec(state)).state);
}

export function mountEditor(parent: HTMLElement): () => void {
  if (!view) {
    view = new EditorView({
      state: activeId ? editorStateOf(activeId) : createState(""),
      dispatchTransactions(trs, v) {
        v.update(trs);
        if (activeId) commit(activeId, v.state);
      },
    });
  }
  parent.appendChild(view.dom);
  view.requestMeasure();
  return () => view?.dom.remove();
}

export function activateEditor(id: TabId): void {
  if (activeId === id && view?.state === states.get(id)) return;
  if (view && activeId) commit(activeId, view.state);
  const next = states.get(id) ?? createState("");
  commit(id, next);
  activeId = id;
  if (view) view.setState(next);
  for (const listener of contextListeners) listener();
}

export function focusEditor(): void {
  view?.focus();
}

export function configureSql(id: TabId, catalog: CatalogView | undefined, databaseKind: DatabaseKind = catalog?.catalog.databaseKind ?? "Postgres"): void {
  updateEditor(id, () => ({ effects: sqlConfig.reconfigure(sqlLanguage(catalog, databaseKind)) }));
}

export function flashRanges(id: TabId, ranges: DocRange[]): void {
  updateEditor(id, () => ({ effects: flashEffect.of(ranges) }));
  setTimeout(() => updateEditor(id, () => ({ effects: clearFlash.of(null) })), 700);
}

/** Boxes the statements a chooser row would run in place of the run box, and scrolls the first into view; `null` clears them. */
export function previewRanges(id: TabId, ranges: DocRange[] | null): void {
  const first = ranges?.[0];
  updateEditor(id, () => ({ effects: first ? [previewEffect.of(ranges), EditorView.scrollIntoView(first.from, { y: "nearest" })] : previewEffect.of(null) }));
}

/** Draws a box behind the statement ⌘↵ would run. */
export function showRunTarget(id: TabId, range: DocRange | null): void {
  updateEditor(id, () => ({ effects: runTargetEffect.of(range) }));
}

/** The viewport's cursor line (the editor's top when the cursor is off screen) and the editor's visible right edge. */
export function cursorAnchor(): { right: number; top: number; bottom: number } | null {
  if (!view) return null;
  const box = view.scrollDOM.getBoundingClientRect();
  const at = view.coordsAtPos(view.state.selection.main.head);
  return { right: box.right, top: at?.top ?? box.top, bottom: at?.bottom ?? box.top };
}

export function showDiagnostics(id: TabId, diagnostics: Diagnostic[]): void {
  updateEditor(id, (state) => setDiagnostics(state, diagnostics));
}

export function moveCursor(id: TabId, pos: number): void {
  updateEditor(id, (state) => ({ selection: { anchor: Math.min(pos, state.doc.length) }, scrollIntoView: true }));
}

export function insertAtCursor(id: TabId, text: string): void {
  updateEditor(id, (state) => {
    const { from, to } = state.selection.main;
    return { changes: { from, to, insert: text }, selection: { anchor: from + text.length } };
  });
}

/** Applies a format in one transaction that history keeps apart from typing, so one undo restores the old text. */
export function applyFormatEdit(id: TabId, { from, to, insert }: FormatEdit): void {
  updateEditor(id, () => ({
    changes: { from, to, insert },
    selection: { anchor: from + insert.length },
    annotations: isolateHistory.of("full"),
    userEvent: "input.format",
    scrollIntoView: true,
  }));
}

/** Swaps the whole document in one transaction. Its userEvent keeps history from joining it with recent typing, so one undo restores the old text. */
export function replaceDoc(id: TabId, text: string): void {
  updateEditor(id, (state) => ({ changes: { from: 0, to: state.doc.length, insert: text }, selection: { anchor: text.length }, userEvent: "input" }));
}
