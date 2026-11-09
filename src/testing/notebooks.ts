import { textOf } from "../features/editor/editorStates";
import type { DatabaseRef, FileBinding, NotebookId, TabId } from "../ipc/bindings";
import { cellMeta, metaKey } from "../state/notebookFile";
import { followActive, notebookOf, settle } from "../state/notebooks";
import { useWorkspace, type Tab, type WorkspaceData } from "../state/workspace";

/** Store fields with `tabs` grouped into a notebook per database in order, showing the notebook that holds `active`. */
export function cellsState(tabs: Tab[], active: TabId | null = tabs[0]?.id ?? null): Pick<WorkspaceData, "tabs" | "notebooks" | "shown"> {
  const byId = Object.fromEntries(tabs.map((t) => [t.id, t]));
  const notebooks = settle(
    [],
    (id) => byId[id]?.console.db,
    tabs.map((t) => t.id),
  );
  const shown = active ? notebookOf(notebooks, active) : undefined;
  if (!shown || !active) return { tabs: byId, notebooks, shown: { kind: "start", db: null } };
  notebooks[shown.id] = { ...shown, active };
  return { tabs: byId, notebooks, shown: { kind: "notebook", id: shown.id } };
}

/** Moves one cell to `db` as a connection edit moves it: into the notebook for its new database, still active if it was. */
export function moveCell(id: TabId, db: DatabaseRef | null): void {
  const s = useWorkspace.getState();
  const tab = s.tabs[id];
  if (!tab) return;
  const tabs = { ...s.tabs, [id]: { ...tab, console: { ...tab.console, db } } };
  useWorkspace.setState({
    tabs,
    ...followActive(
      s.shown,
      s.notebooks,
      settle(Object.values(s.notebooks), (c) => tabs[c]?.console.db),
    ),
  });
}

/** Binds a notebook to `path` as if it had just been opened from that file, so it reads saved. */
export function bindToFile(id: NotebookId, path = "/q/a.sql", plain = false): FileBinding {
  const { notebooks, tabs } = useWorkspace.getState();
  const binding: FileBinding = { path, stamp: "s1", format: { bom: false, crlf: false } };
  const saved = (notebooks[id]?.cells ?? []).map((c) => ({ text: textOf(c), meta: metaKey(cellMeta(tabs[c]!)) }));
  useWorkspace.getState().setNotebookFile(id, { binding, health: "ok", plain, unknown: { header: [], cells: {} }, saved });
  return binding;
}
