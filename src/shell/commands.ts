import { closeTab } from "../features/editor/closeTab";
import { openFiles, saveTab, type SaveMode } from "../features/editor/fileActions";
import { isBrowsing, openSqlTab, submitEdits } from "../features/editor/runActions";
import { saveActiveQuery } from "../features/saved/savedActions";
import { openNavigation } from "../features/navigation/navigationActions";
import { pendingCount, useTableEdits } from "../state/tableEdits";
import { activeId, activeTab, shownDb, useWorkspace, type Overlay } from "../state/workspace";
import { isMac } from "./hostedWindow";

export type CommandGroup = "Query" | "Connections" | "Workspace";
export const COMMAND_GROUPS: CommandGroup[] = ["Query", "Connections", "Workspace"];

export type Command = {
  group: CommandGroup;
  id: string;
  label: string;
  key?: string;
  shift?: boolean;
  available?: () => boolean;
  labelForContext?: () => string;
  palette?: false;
  run: () => void;
};

const MAC = typeof navigator !== "undefined" && isMac(navigator.userAgent);

const ws = () => useWorkspace.getState();

const inWorkspace = (run: () => void) => () => {
  ws().closeSettings();
  run();
};

const open = (overlay: Overlay) => inWorkspace(() => ws().open(overlay));

export function openCommandPalette(): void {
  const workspace = ws();
  if (workspace.overlay.kind === "commands") document.querySelector<HTMLInputElement>('[data-testid="command-input"]')?.focus();
  else if (workspace.overlay.kind === "none") workspace.open({ kind: "commands" });
}

const hasWorkspaceTab = () => {
  return ws().screen.kind === "workspace" && activeTab(ws()) !== undefined;
};

function toggleSaved() {
  const wasOpen = ws().overlay.kind === "saved";
  ws().closeSettings();
  if (!wasOpen) ws().open({ kind: "saved", entry: { kind: "browse" } });
}

const startsNotebook = () => ws().shown.kind === "start" && shownDb(ws()) !== null;

export const START_COMMANDS: Command[] = [
  {
    id: "new-query",
    group: "Query",
    label: "New query tab",
    labelForContext: () => (startsNotebook() ? "New notebook" : "New query tab"),
    key: "t",
    run: inWorkspace(() => openSqlTab()),
  },
  { id: "open-file", group: "Query", label: "Open file…", key: "o", run: () => void openFiles() },
  { id: "new-connection", group: "Connections", label: "New connection", key: "n", run: open({ kind: "connection", editing: null }) },
  { id: "navigation", group: "Workspace", label: "Quick navigation", key: "p", run: openNavigation },
  { id: "saved-queries", group: "Query", label: "Saved queries", key: "e", run: toggleSaved },
  {
    id: "settings",
    group: "Workspace",
    label: "Settings",
    labelForContext: () => (ws().screen.kind === "settings" ? "Close Settings" : "Settings"),
    key: ",",
    run: () => (ws().screen.kind === "settings" ? ws().closeSettings() : ws().openSettings()),
  },
];

const CLOSE_TAB: Command = {
  id: "close-tab",
  group: "Query",
  label: "Close tab",
  key: "w",
  labelForContext: () => (ws().screen.kind === "settings" ? "Close Settings" : "Close tab"),
  available: () => ws().screen.kind === "settings" || hasWorkspaceTab(),
  run: () => {
    const workspace = ws();
    const { screen, closeSettings } = workspace;
    const active = activeId(workspace);
    if (screen.kind === "settings") closeSettings();
    else if (active)
      void closeTab(active).then(() => {
        if (ws().tabs[active]) return;
        requestAnimationFrame(() => {
          const selected = document.querySelector<HTMLButtonElement>('[role="tab"][aria-selected="true"]');
          const start = document.querySelector<HTMLButtonElement>('[data-testid="start-new-query"]');
          (selected ?? start)?.focus();
        });
      });
  },
};

/**
 * A save acts on the workspace screen with no dialog up, so a save dialog never stacks on another one. With staged
 * table edits in the active tab, ⌘S submits them wherever focus is.
 */
const saveActive = (mode: SaveMode) => () => {
  const workspace = ws();
  const { screen, overlay } = workspace;
  const active = activeId(workspace);
  if (screen.kind !== "workspace" || overlay.kind !== "none" || !active) return;
  const edits = useTableEdits.getState().byTab[active];
  if (mode === "save" && edits && pendingCount(edits) > 0) void submitEdits(active);
  else void saveTab(active, mode);
};

const SAVE: Command = { id: "save", group: "Query", label: "Save", key: "s", available: hasWorkspaceTab, run: saveActive("save") };
const SAVE_AS: Command = { id: "save-as", group: "Query", label: "Save as…", key: "s", shift: true, available: hasWorkspaceTab, run: saveActive("saveAs") };

const SAVE_QUERY: Command = {
  id: "save-query",
  group: "Query",
  label: "Save query",
  key: "e",
  shift: true,
  available: () => {
    const tab = activeTab(ws());
    return hasWorkspaceTab() && tab !== undefined && !isBrowsing(tab);
  },
  run: () => void saveActiveQuery(),
};

export const COMMANDS: Command[] = [
  ...START_COMMANDS,
  {
    id: "reset-panels",
    group: "Workspace",
    label: "Reset panel sizes",
    run: () => ws().setLayout({ sidebarWidth: 240, rightPanelWidth: 360, resultsHeight: 320 }),
  },
  CLOSE_TAB,
  SAVE,
  SAVE_AS,
  SAVE_QUERY,
  { id: "commands", group: "Workspace", label: "Command panel", key: "p", shift: true, palette: false, run: openCommandPalette },
];

export const commandLabel = (command: Command): string => command.labelForContext?.() ?? command.label;
export const commandAvailable = (command: Command): boolean => command.available?.() ?? true;

/** On macOS a command takes ⌘ alone, because the editor owns the Ctrl chords there (Ctrl-T transposes, Ctrl-P moves up). */
export function commandForKey(e: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey">, mac = MAC): Command | undefined {
  const mod = mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  if (!mod || e.altKey) return undefined;
  const key = e.key.toLowerCase();
  return COMMANDS.find((c) => c.key === key && !!c.shift === e.shiftKey);
}

/**
 * The window's keydown listener. A control that handles a chord itself calls `preventDefault`, and the global
 * command then stands down, so the chord never fires twice.
 */
export function handleGlobalKey(e: KeyboardEvent): void {
  if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
  const command = commandForKey(e);
  if (!command) return;
  e.preventDefault();
  if (commandAvailable(command)) command.run();
}

export function shortcutLabel(c: Command): string {
  if (!c.key) return "";
  return MAC ? `⌘${c.shift ? "⇧" : ""}${c.key.toUpperCase()}` : `Ctrl+${c.shift ? "Shift+" : ""}${c.key.toUpperCase()}`;
}
