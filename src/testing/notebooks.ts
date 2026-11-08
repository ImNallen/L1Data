import type { DatabaseRef, TabId } from "../ipc/bindings";
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
