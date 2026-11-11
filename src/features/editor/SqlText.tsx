import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { sql } from "@codemirror/lang-sql";
import { EditorState, Prec } from "@codemirror/state";
import { drawSelection, EditorView, keymap, lineNumbers } from "@codemirror/view";
import { useEffect, useRef } from "react";
import type { DatabaseKind } from "../../ipc/bindings";
import { SQL_DIALECT } from "./sqlDialect";
import { editorHighlighting, editorTheme, functionCallStyle } from "./theme";

/** `wrap` folds long lines at the pane edge. A notebook cell keeps them whole, as its editor does. */
type Common = { databaseKind: DatabaseKind; label: string; testId: string; wrap: boolean };

export type SqlTextProps =
  | (Common & { mode: "view"; text: string })
  | (Common & { mode: "edit"; initial: string; autoFocus: boolean; onChange(text: string): void; onSubmit(): void });

export function SqlText(props: SqlTextProps) {
  const host = useRef<HTMLDivElement>(null);
  const latest = useRef(props);
  // oxlint-disable-next-line react/refs -- the editor's listeners read the newest props, and nothing renders from this ref
  latest.current = props;
  const { mode, databaseKind, label, wrap } = props;
  const doc = props.mode === "view" ? props.text : props.initial;
  useEffect(() => {
    if (!host.current) return;
    const submit = () => {
      const p = latest.current;
      if (p.mode === "edit") p.onSubmit();
      return true;
    };
    const behavior =
      mode === "view"
        ? [EditorState.readOnly.of(true), EditorView.editable.of(false)]
        : [
            history(),
            Prec.highest(
              keymap.of([
                { key: "Mod-Enter", run: submit },
                { key: "Mod-s", run: submit },
              ]),
            ),
            keymap.of([...defaultKeymap, ...historyKeymap]),
            EditorView.updateListener.of((u) => {
              const p = latest.current;
              if (u.docChanged && p.mode === "edit") p.onChange(u.state.doc.toString());
            }),
          ];
    const view = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc,
        extensions: [
          behavior,
          EditorView.contentAttributes.of({ "aria-label": label, tabindex: "0" }),
          lineNumbers(),
          drawSelection(),
          wrap ? EditorView.lineWrapping : [],
          sql({ dialect: SQL_DIALECT[databaseKind] }),
          editorTheme,
          editorHighlighting,
          functionCallStyle,
        ],
      }),
    });
    const p = latest.current;
    if (p.mode === "edit" && p.autoFocus) {
      view.dispatch({ selection: { anchor: view.state.doc.length } });
      view.focus();
    }
    return () => view.destroy();
  }, [mode, doc, databaseKind, label, wrap]);
  return <div ref={host} data-testid={props.testId} className="h-full min-h-0" />;
}
