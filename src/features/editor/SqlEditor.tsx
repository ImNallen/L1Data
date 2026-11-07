import { useEffect, useLayoutEffect, useRef } from "react";
import type { TabId } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { databaseKindFor } from "../../state/databaseKind";
import { tabTarget, useWorkspace } from "../../state/workspace";
import { activateEditor, configureSql, cursorOf, editorStateOf, focusEditor, mountEditor } from "./editorStates";

export function SqlEditor({ tabId, autoFocus = true }: { tabId: TabId; autoFocus?: boolean }) {
  const host = useRef<HTMLDivElement>(null);
  const tab = useWorkspace((s) => s.tabs[tabId]);
  const target = tabTarget(tab);
  const databaseKind = useWorkspace((s) => databaseKindFor(target, s.configs));
  const view = useCatalog((s) => viewFor(s.views, target));

  useLayoutEffect(() => {
    activateEditor(tabId);
    useWorkspace.getState().setCursor(cursorOf(editorStateOf(tabId)));
  }, [tabId]);

  useEffect(() => {
    if (!host.current) return;
    return mountEditor(host.current);
  }, []);

  useEffect(() => {
    configureSql(tabId, view, databaseKind);
  }, [tabId, view, databaseKind]);

  useEffect(() => {
    if (autoFocus) focusEditor();
  }, [tabId, autoFocus]);

  return <div ref={host} data-testid="sql-editor" className="h-full min-h-0 bg-bg" />;
}
