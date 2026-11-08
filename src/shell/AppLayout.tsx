import { useEffect, useState } from "react";
import { Group, Panel } from "react-resizable-panels";
import { useDatabaseMap } from "../features/databaseMap/controller";
import { NotebookWorkspace } from "../features/editor/NotebookWorkspace";
import { StatementChooser } from "../features/editor/StatementChooser";
import { HistoryPalette } from "../features/history/HistoryPalette";
import { NavigationPalette } from "../features/navigation/NavigationPalette";
import { NotebookSwitcher } from "../features/notebooks/NotebookSwitcher";
import { SavedPalette } from "../features/saved/SavedPalette";
import { Sidebar } from "../features/sidebar/Sidebar";
import { InstallingOverlay } from "../features/updates/InstallingOverlay";
import { useWorkspace } from "../state/workspace";
import { handleGlobalKey } from "./commands";
import { CommandPalette } from "./CommandPalette";
import { ConnectionDialog, DatabaseMap, SettingsDialog, useMountOnce, useSurfacesReady } from "./lazySurfaces";
import { Overlays } from "./Overlays";
import { PixelColumn } from "./PixelColumn";
import { ResizeHandle } from "./ResizeHandle";
import { RightPanel } from "./RightPanel";
import { ResizeEdges } from "./WindowChrome";
import { hostedWindow } from "./hostedWindow";
import { TopBar } from "./TopBar";

function useGlobalKeys() {
  useEffect(() => {
    window.addEventListener("keydown", handleGlobalKey);
    return () => window.removeEventListener("keydown", handleGlobalKey);
  }, []);
}

export function AppLayout() {
  useGlobalKeys();
  const [hosted] = useState(() => hostedWindow());
  const sidebar = useWorkspace((s) => s.layout.sidebar);
  const rightPanel = useWorkspace((s) => s.layout.rightPanel);
  const mapOpen = useDatabaseMap((s) => s.map.kind !== "closed");
  const surfacesReady = useSurfacesReady();
  const settingsMounted = useMountOnce(
    useWorkspace((s) => s.screen.kind === "settings"),
    surfacesReady,
  );
  const connectionMounted = useMountOnce(
    useWorkspace((s) => s.overlay.kind === "connection"),
    surfacesReady,
  );

  return (
    <div className="relative flex h-full">
      {hosted?.chrome === "custom" && <ResizeEdges hosted={hosted} />}
      <PixelColumn column="sidebar" open={sidebar}>
        <Sidebar hosted={hosted} />
      </PixelColumn>
      <main data-testid="main" className="workspace-main flex min-w-80 flex-1 flex-col">
        <TopBar hosted={hosted} notebook />
        <Group orientation="horizontal" className="min-h-0 flex-1">
          <Panel id="sql-work" minSize={240}>
            <NotebookWorkspace />
          </Panel>
          {mapOpen && (
            <>
              <ResizeHandle id="map-resize" label="Resize database map" line="vertical" lineColor="bg-border" />
              <Panel id="database-map" defaultSize="60%" minSize={340}>
                <DatabaseMap />
              </Panel>
            </>
          )}
        </Group>
      </main>
      <PixelColumn column="right" open={rightPanel !== null}>
        <RightPanel hosted={hosted} />
      </PixelColumn>
      {settingsMounted && <SettingsDialog />}
      <NavigationPalette />
      <NotebookSwitcher />
      <CommandPalette />
      <HistoryPalette />
      <SavedPalette />
      {connectionMounted && <ConnectionDialog />}
      <StatementChooser />
      <Overlays />
      <InstallingOverlay />
    </div>
  );
}
