import { useEffect, useRef, useState } from "react";
import { Group, Panel, type PanelImperativeHandle } from "react-resizable-panels";
import { useDatabaseMap } from "../features/databaseMap/controller";
import { SqlEditor } from "../features/editor/SqlEditor";
import { StatementChooser } from "../features/editor/StatementChooser";
import { HistoryPalette } from "../features/history/HistoryPalette";
import { NavigationPalette } from "../features/navigation/NavigationPalette";
import { SavedPalette } from "../features/saved/SavedPalette";
import { ResultsPane } from "../features/results/ResultsPane";
import { Sidebar } from "../features/sidebar/Sidebar";
import { InstallingOverlay } from "../features/updates/InstallingOverlay";
import { isTableTab, useWorkspace } from "../state/workspace";
import { Kbd } from "../ui/primitives";
import { START_COMMANDS, handleGlobalKey, shortcutLabel } from "./commands";
import { CommandPalette } from "./CommandPalette";
import { ConnectionDialog, DatabaseMap, SettingsDialog, useMountOnce, useSurfacesReady } from "./lazySurfaces";
import { Overlays } from "./Overlays";
import { PixelColumn } from "./PixelColumn";
import { ResizeHandle } from "./ResizeHandle";
import { RightPanel } from "./RightPanel";
import { TabBar } from "./TabBar";
import { ResizeEdges } from "./WindowChrome";
import { hostedWindow } from "./hostedWindow";
import { TopBar } from "./TopBar";

function useGlobalKeys() {
  useEffect(() => {
    window.addEventListener("keydown", handleGlobalKey);
    return () => window.removeEventListener("keydown", handleGlobalKey);
  }, []);
}

function StartCommands() {
  return (
    <div data-testid="no-tabs" className="flex h-full flex-col overflow-y-auto bg-bg p-5">
      <div className="mx-auto my-auto w-full max-w-[320px] shrink-0">
        <div className="mb-3 px-2.5">
          <h1 className="text-ui-14 font-semibold text-fg">Your SQL workspace</h1>
          <p className="mt-1 text-ui-13 text-dim">Open a query or connect to a database to begin.</p>
        </div>
        <ul className="flex flex-col gap-0.5">
          {START_COMMANDS.map((c, i) => (
            <li key={c.id} className={i === 3 ? "mt-2 border-t border-border pt-2" : undefined}>
              <button
                type="button"
                data-testid={`start-${c.id}`}
                onClick={c.run}
                className={`flex min-h-8 w-full items-center justify-between gap-3 rounded-md px-2.5 py-1 text-ui-13.5 hover:bg-hover hover:text-fg focus-visible:outline-2 focus-visible:outline-focus ${i === 0 ? "text-fg" : "text-muted"}`}
              >
                {c.label}
                <Kbd className="shrink-0 text-muted">{shortcutLabel(c)}</Kbd>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

const TABLE_EDITOR_PX = 96;

function MainArea() {
  const active = useWorkspace((s) => (s.active ? s.tabs[s.active] : undefined));
  const tableTab = isTableTab(active);
  const showResults = useWorkspace((s) => s.layout.results);
  const resultsHeight = useWorkspace.getState().layout.resultsHeight;
  const resultsPanel = useRef<PanelImperativeHandle | null>(null);
  const resultsPx = useRef<number | null>(null);
  useEffect(() => {
    return useWorkspace.subscribe((state, previous) => {
      if (tableTab || state.layout.resultsHeight === previous.layout.resultsHeight) return;
      resultsPx.current = state.layout.resultsHeight;
      resultsPanel.current?.resize(state.layout.resultsHeight);
    });
  }, [tableTab]);
  // Saved on release only: a store update mid-drag re-renders the Group and the library drops the drag.
  const saveResultsHeight = () => {
    const { layout, setLayout } = useWorkspace.getState();
    if (resultsPx.current !== null && resultsPx.current !== layout.resultsHeight) setLayout({ resultsHeight: resultsPx.current });
  };

  // A Panel reads defaultSize only on mount, so switching between a table tab and a query tab remounts the split.
  return (
    <Group key={tableTab ? "table" : "query"} orientation="vertical" className="min-h-0 flex-1" onLayoutChanged={tableTab ? undefined : saveResultsHeight}>
      <Panel
        id="editor"
        minSize={80}
        defaultSize={tableTab && showResults ? TABLE_EDITOR_PX : undefined}
        groupResizeBehavior={tableTab ? "preserve-pixel-size" : "preserve-relative-size"}
      >
        {active ? <SqlEditor tabId={active.id} /> : <StartCommands />}
      </Panel>
      {showResults && (
        <>
          <ResizeHandle id="results-resize" label="Resize results" line="horizontal" lineColor="bg-border" />
          <Panel
            id="results"
            panelRef={resultsPanel}
            defaultSize={tableTab ? undefined : resultsHeight}
            minSize={120}
            groupResizeBehavior={tableTab ? "preserve-relative-size" : "preserve-pixel-size"}
            onResize={(size) => {
              if (!tableTab) resultsPx.current = Math.round(size.inPixels);
            }}
          >
            <ResultsPane tabId={active?.id ?? null} />
          </Panel>
        </>
      )}
    </Group>
  );
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
      <main data-testid="main" className="flex min-w-80 flex-1 flex-col bg-bg">
        <TopBar hosted={hosted} />
        <TabBar />
        <Group orientation="horizontal" className="min-h-0 flex-1">
          <Panel id="sql-work" minSize={240}>
            <MainArea />
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
