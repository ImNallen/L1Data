import type { TabId } from "../../ipc/bindings";
import { homeDb, resolveDb, sameDb } from "../../state/dbKey";
import { instanceOf, useLive } from "../../state/live";
import { notebookFor } from "../../state/notebooks";
import { activeId, useWorkspace } from "../../state/workspace";
import { focusEditor } from "../editor/editorStates";
import { openSqlTab, openTableTab } from "../editor/runActions";
import { type Destination } from "./navigationItems";

export function openNavigation(): void {
  const workspace = useWorkspace.getState();
  if (workspace.overlay.kind === "navigation") document.querySelector<HTMLInputElement>('[data-testid="navigation-input"]')?.focus();
  else workspace.open({ kind: "navigation" });
}

export async function activateDestination(destination: Destination): Promise<TabId | null> {
  const workspace = useWorkspace.getState();
  workspace.close();
  switch (destination.kind) {
    case "tab":
      if (!workspace.tabs[destination.tabId]) return null;
      workspace.closeSettings();
      workspace.setActive(destination.tabId);
      return destination.tabId;
    case "database": {
      const { db } = destination;
      const config = workspace.configs[db.connectionId];
      const connection = useLive.getState().connections[db.connectionId];
      const instance = instanceOf(connection);
      const available = config && (instance ? instance.databases.some((d) => d.name === db.database && d.access === "Ok") : sameDb(homeDb(config), db));
      if (!available || !sameDb(db, resolveDb(db, connection))) {
        workspace.notify("This database is no longer available. Open quick navigation again to choose a database.");
        return null;
      }
      workspace.closeSettings();
      if (!notebookFor(workspace.notebooks, db)) return openSqlTab({ db });
      workspace.showDatabase(db);
      return activeId(useWorkspace.getState());
    }
    case "relation":
      workspace.closeSettings();
      return openTableTab(destination.ref);
    default: {
      const unreachable: never = destination;
      return unreachable;
    }
  }
}

export function focusDestination(tabId: TabId): void {
  requestAnimationFrame(() => {
    const workspace = useWorkspace.getState();
    if (activeId(workspace) !== tabId || workspace.overlay.kind !== "none" || workspace.screen.kind !== "workspace") return;
    const grid = workspace.tabs[tabId]?.table && document.querySelector<HTMLElement>('[data-testid="result-grid"]');
    if (grid) grid.focus();
    else focusEditor();
  });
}
