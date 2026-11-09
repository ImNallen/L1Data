import { listen } from "@tauri-apps/api/event";
import { commands, type AppEvent } from "./ipc/bindings";
import { useCatalog } from "./state/catalog";
import { useLive, wantedCatalogs, type LiveData } from "./state/live";
import { useRelationDetail } from "./state/relationDetail";
import { tabFromSnapshot, tabTarget, useWorkspace } from "./state/workspace";
import { startAppearance } from "./shell/appearance";
import { startTheme } from "./shell/theme";
import { seedEditor } from "./features/editor/editorStates";
import { reconcileNotebookFiles, savesInFlight } from "./features/notebooks/notebookFiles";
import { installCloseFlush, startPersistence } from "./features/editor/persistence";
import { browseTemplate, installRunBehavior, openSqlTab } from "./features/editor/runActions";
import { startAssistantSync } from "./features/assistant/assistantActions";
import { startUpdateSync } from "./features/updates/updateActions";
import { unreadableConnectionsNotice } from "./features/connections/connectionForm";

/** The catalog store holds exactly the catalogs the connection snapshots serve, and the detail store only their relations. */
function startCatalogSync() {
  const sync = (s: LiveData) => {
    const wanted = wantedCatalogs(s.connections);
    const catalog = useCatalog.getState();
    catalog.prune(new Set(wanted.keys()));
    for (const { ref, version } of wanted.values()) void catalog.ensure(ref, version);
  };
  useLive.subscribe((s, prev) => {
    if (s.connections !== prev.connections) sync(s);
  });
  useCatalog.subscribe((s, prev) => {
    if (s.views !== prev.views) useRelationDetail.getState().prune(s.views);
  });
  sync(useLive.getState());
}

export async function boot(): Promise<void> {
  installRunBehavior();
  await listen<AppEvent>("app-event", (e) => useLive.getState().apply(e.payload));
  const snapshot = await commands.appBoot();
  useLive.getState().hydrate(snapshot);
  const configs = Object.fromEntries(snapshot.connections.map((config) => [config.id, config]));
  for (const saved of snapshot.workspace.tabs) {
    const tab = tabFromSnapshot(saved, configs);
    const db = tabTarget(tab);
    const template = tab?.table && db ? await browseTemplate(db, tab.table.schema, tab.table.table) : null;
    seedEditor(saved.id, saved.text ?? template ?? "");
  }
  useWorkspace.getState().restore(snapshot.workspace, snapshot.connections, snapshot.settings);
  const unreadable = unreadableConnectionsNotice(snapshot.unreadableConnections);
  if (unreadable) useWorkspace.getState().notify(unreadable);
  useWorkspace.getState().resolveDatabases(useLive.getState().connections);
  useLive.subscribe((s, prev) => {
    if (s.connections !== prev.connections) useWorkspace.getState().resolveDatabases(s.connections);
  });
  startTheme();
  startAppearance();
  if (snapshot.workspace.tabs.length === 0) openSqlTab();
  startPersistence();
  installCloseFlush(savesInFlight);
  void reconcileNotebookFiles();
  startCatalogSync();
  startAssistantSync();
  startUpdateSync();
}
