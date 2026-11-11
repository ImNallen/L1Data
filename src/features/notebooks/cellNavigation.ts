import { completionStatus } from "@codemirror/autocomplete";
import { Prec, type EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { flushSync } from "react-dom";
import type { TabId } from "../../ipc/bindings";
import { adjacentCell, notebookOf } from "../../state/notebooks";
import { activeId, useWorkspace } from "../../state/workspace";
import { activateEditor, editorStateOf, focusEditor, moveCursor } from "../editor/editorStates";

/** Makes `id` the active cell, expanded, with the editor focused in it, and puts its cursor at `at` when given. */
export function goToCell(id: TabId, at?: (state: EditorState) => number): void {
  // React moves the editor into the cell on commit, and the move drops focus, so commit now and focus after it.
  flushSync(() => {
    useWorkspace.getState().setCollapsed([id], false);
    useWorkspace.getState().setActive(id);
  });
  activateEditor(id);
  if (at) moveCursor(id, at(editorStateOf(id)));
  focusEditor();
}

/** Puts keyboard focus on the active cell: in its editor, or on its title while it is collapsed and has no editor. */
export function focusActiveCell(): void {
  const s = useWorkspace.getState();
  const id = activeId(s);
  if (!id) return;
  if (s.tabs[id]?.collapsed) document.querySelector<HTMLElement>(`button[data-tab-id="${CSS.escape(id)}"]`)?.focus();
  else focusEditor();
}

const STEPS: Partial<Record<string, -1 | 1>> = { ArrowUp: -1, ArrowDown: 1 };

/** Moves to the neighboring cell, at the same column, when the cursor sits on the edge line facing it. */
function crossCell({ state }: EditorView, step: -1 | 1): boolean {
  const { main } = state.selection;
  const line = state.doc.lineAt(main.head);
  const edge = step < 0 ? 1 : state.doc.lines;
  if (state.selection.ranges.length > 1 || !main.empty || line.number !== edge || completionStatus(state) === "active") return false;
  const s = useWorkspace.getState();
  const from = activeId(s);
  const to = from ? adjacentCell(notebookOf(s.notebooks, from)?.cells ?? [], from, step) : undefined;
  if (!to) return false;
  const col = main.head - line.from;
  goToCell(to, ({ doc }) => {
    const target = doc.line(step < 0 ? doc.lines : 1);
    return target.from + Math.min(col, target.length);
  });
  return true;
}

export const cellNavigation = Prec.highest(
  EditorView.domEventHandlers({
    keydown(event, view) {
      const step = event.shiftKey || event.altKey || event.metaKey || event.ctrlKey || event.isComposing ? undefined : STEPS[event.key];
      if (!step || !crossCell(view, step)) return false;
      // The notebook lets go of a gliding cell on an arrow key, but this arrow started the glide. A keymap binding's
      // stopPropagation would also stop the arrows that move within a cell.
      event.stopPropagation();
      return true;
    },
  }),
);
