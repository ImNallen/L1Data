import { useEffect, useState } from "react";
import { Group, Panel } from "react-resizable-panels";
import { useDatabaseMap } from "../features/databaseMap/controller";
import { NotebookWorkspace } from "../features/editor/NotebookWorkspace";
import { StatementChooser } from "../features/editor/StatementChooser";
import { NavigationPalette } from "../features/navigation/NavigationPalette";
import { NotebookSwitcher } from "../features/notebooks/NotebookSwitcher";
import { QueriesDialog } from "../features/queries/QueriesDialog";
import { Sidebar } from "../features/sidebar/Sidebar";
import { InstallingOverlay } from "../features/updates/InstallingOverlay";
import { useWorkspace } from "../state/workspace";
import { cx } from "../ui/primitives";
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
import { SidebarToggle, WINDOW_HEADER_HEIGHT, WindowEdgeActions, windowEdgeInset, windowLeadingInset } from "./WindowEdgeActions";

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
      <div className={cx("pointer-events-none absolute top-0 left-0 z-20 flex items-center", WINDOW_HEADER_HEIGHT, windowLeadingInset(hosted))}>
        <div className="pointer-events-auto flex items-center">
          <SidebarToggle />
        </div>
      </div>
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
      <div className={cx("pointer-events-none absolute top-0 right-0 z-20 flex items-center", WINDOW_HEADER_HEIGHT, windowEdgeInset(hosted))}>
        <div className="pointer-events-auto flex items-center">
          <WindowEdgeActions hosted={hosted} />
        </div>
      </div>
      <PixelColumn column="right" open={rightPanel !== null}>
        <RightPanel hosted={hosted} />
      </PixelColumn>
      {settingsMounted && <SettingsDialog />}
      <NavigationPalette />
      <NotebookSwitcher />
      <CommandPalette />
      <QueriesDialog />
      {connectionMounted && <ConnectionDialog />}
      <StatementChooser />
      <Overlays />
      <InstallingOverlay />
    </div>
  );
}
