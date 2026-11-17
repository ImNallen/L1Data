import * as Dialog from "@radix-ui/react-dialog";
import { NetworkIcon, XIcon } from "lucide-react";
import { useRef, useState } from "react";
import type { TabId } from "../../ipc/bindings";
import { activeId, isMapCell, useWorkspace, type WorkspaceData } from "../../state/workspace";
import { DialogSurface, IconButton } from "../../ui/primitives";
import { EnvironmentText } from "../connections/connectionColors";
import { focusActiveCell } from "../notebooks/cellNavigation";
import { CanvasStatus, findShortcut, MapLegend, MapToolbar } from "./MapCell";
import { MapInspector } from "./MapInspector";
import { SchemaCanvas } from "./SchemaCanvas";
import { useSchemaMap } from "./useSchemaMap";
import { clearFocusIfAny } from "./views";

const ws = () => useWorkspace.getState();

export const fullViewButtonId = (id: TabId) => `map-full-view-${id}`;
const canvasId = (id: TabId) => `map-full-${id}`;

export function openFullView(id: TabId): void {
  ws().setActive(id);
  ws().open({ kind: "schemaMap", tab: id });
}

const openCell = (s: WorkspaceData): TabId | null => (s.overlay.kind === "schemaMap" && isMapCell(s.tabs[s.overlay.tab]) ? s.overlay.tab : null);

function FullView({ id, open }: { id: TabId; open: boolean }) {
  const map = useSchemaMap(id, open);
  const leaving = useRef(false);
  const config = useWorkspace((s) => (map?.db ? s.configs[map.db.connectionId] : undefined));
  const find = useRef<HTMLInputElement>(null);
  if (!map) return null;
  const { db } = map;
  const title = `${db?.database ?? "No database"} · ${map.schema ?? "default schema"}`;
  const closeThen = (go: () => Promise<void>) => {
    leaving.current = true;
    ws().close();
    void go().then(focusActiveCell);
  };
  return (
    <DialogSurface
      data-testid="schema-map-dialog"
      aria-describedby={undefined}
      className="flex h-[calc(100vh-48px)] w-[min(1440px,calc(100vw-48px))] flex-col overflow-hidden"
      onOpenAutoFocus={(e) => {
        e.preventDefault();
        document.querySelector<HTMLElement>(`[data-testid="${canvasId(id)}"]`)?.focus();
      }}
      // Radix handles Escape in a document capture listener, before the find box's own handler can claim it.
      onEscapeKeyDown={(e) => e.preventDefault()}
      onKeyDown={(e) => {
        if (findShortcut(e, find)) return;
        if (e.key !== "Escape" || !(e.target instanceof Node) || !e.currentTarget.contains(e.target)) return;
        e.preventDefault();
        if (!clearFocusIfAny(id)) ws().close();
      }}
      onCloseAutoFocus={(e) => {
        e.preventDefault();
        if (leaving.current) {
          leaving.current = false;
          return;
        }
        const workspace = ws();
        if (workspace.overlay.kind !== "none" || activeId(workspace) !== id) return;
        document.querySelector<HTMLElement>(`[data-testid="${fullViewButtonId(id)}"]`)?.focus();
      }}
    >
      <header className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border pr-2 pl-4">
        <NetworkIcon size={15} aria-hidden className="shrink-0 text-keyword" />
        <Dialog.Title className="shrink-0 text-ui-13 font-semibold text-fg">{title}</Dialog.Title>
        {config && <span className="min-w-0 truncate text-ui-12 text-muted">{config.name}</span>}
        <EnvironmentText environment={config?.environment ?? null} testId="map-full-environment" />
        <MapToolbar map={map} findRef={find} className="ml-auto border-0 p-0" />
        <span aria-hidden className="mx-0.5 h-5 w-px shrink-0 bg-control" />
        <Dialog.Close asChild>
          <IconButton aria-label="Close full view" title="Close full view">
            <XIcon size={15} />
          </IconButton>
        </Dialog.Close>
      </header>
      <div className="flex min-h-0 flex-1">
        <div className="relative min-w-0 flex-1">
          <SchemaCanvas
            map={map}
            mode={{ opens: "actualSize", wheel: "pans" }}
            height="fill"
            testId={canvasId(id)}
            label={`Schema map of ${map.schema ?? "the default schema"}`}
            gestureHint={
              map.focus
                ? "Esc clears the focus, then closes · Click outside to close"
                : "Esc or click outside to close · Drag to pan · ⌘ scroll to zoom · Click a table to focus it"
            }
          />
          <CanvasStatus map={map} />
        </div>
        {map.focus && <MapInspector map={map} closeThen={closeThen} />}
      </div>
      <div className="shrink-0 border-t border-border">
        <MapLegend map={map} />
      </div>
    </DialogSurface>
  );
}

export function SchemaMapDialog() {
  const open = useWorkspace(openCell);
  const [lastOpenCell, setLastOpenCell] = useState<TabId | null>(null);
  if (open && open !== lastOpenCell) setLastOpenCell(open);
  return (
    <Dialog.Root open={open !== null} onOpenChange={(value) => !value && ws().close()}>
      <Dialog.Portal>{lastOpenCell && <FullView key={lastOpenCell} id={lastOpenCell} open={open !== null} />}</Dialog.Portal>
    </Dialog.Root>
  );
}
