import { closeTab } from "../features/editor/closeTab";
import { isBrowsing, openSqlTab, submitEdits } from "../features/editor/runActions";
import { saveActiveQuery } from "../features/saved/savedActions";
import { openNavigation } from "../features/navigation/navigationActions";
import { openNotebookSwitcher } from "../features/notebooks/notebookActions";
import { openNotebookFiles, saveNotebook, type SaveMode } from "../features/notebooks/notebookFiles";
import { adjacentCell } from "../state/notebooks";
import { pendingCount, useTableEdits } from "../state/tableEdits";
import { activeId, activeTab, shownDb, shownNotebook, useWorkspace, type Overlay } from "../state/workspace";
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
  const { overlay, screen } = ws();
  if (overlay.kind === "queries" && overlay.tab === "saved") return ws().close();
  if (screen.kind === "settings") ws().closeSettings();
  ws().openQueries("saved");
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
  { id: "open-file", group: "Query", label: "Open notebook file…", key: "o", run: () => void openNotebookFiles() },
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

export const SWITCH_NOTEBOOK: Command = { id: "switch-notebook", group: "Workspace", label: "Switch notebook…", run: openNotebookSwitcher };

const startCommand = (id: string) => START_COMMANDS.find((c) => c.id === id)!;

/** The start commands in their sections: a database with no notebook offers notebooks, no database offers everything. */
export function startSections(hasDatabase: boolean): Command[][] {
  const ids = hasDatabase
    ? [
        ["new-query", "open-file"],
        ["navigation", "saved-queries"],
      ]
    : [
        ["new-query", "open-file", "new-connection"],
        ["navigation", "saved-queries", "settings"],
      ];
  const sections = ids.map((section) => section.map(startCommand));
  if (hasDatabase) sections[0]!.push(SWITCH_NOTEBOOK);
  return sections;
}

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
 * A save writes the shown notebook, and acts on the workspace screen with no dialog up, so a save dialog never stacks on
 * another one. With staged table edits in the active cell, ⌘S submits them wherever focus is.
 */
const saveShown = (mode: SaveMode) => () => {
  const workspace = ws();
  const { screen, overlay } = workspace;
  const active = activeId(workspace);
  const notebook = shownNotebook(workspace);
  if (screen.kind !== "workspace" || overlay.kind !== "none" || !active || !notebook) return;
  const edits = useTableEdits.getState().byTab[active];
  if (mode === "save" && edits && pendingCount(edits) > 0) void submitEdits(active);
  else void saveNotebook(notebook.id, mode);
};

const SAVE: Command = { id: "save", group: "Query", label: "Save notebook", key: "s", available: hasWorkspaceTab, run: saveShown("save") };
const SAVE_AS: Command = {
  id: "save-as",
  group: "Query",
  label: "Save notebook as…",
  key: "s",
  shift: true,
  available: hasWorkspaceTab,
  run: saveShown("saveAs"),
};

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

const COLLAPSE_CELL: Command = {
  id: "collapse-cell",
  group: "Query",
  label: "Collapse cell",
  labelForContext: () => (activeTab(ws())?.collapsed ? "Expand cell" : "Collapse cell"),
  available: hasWorkspaceTab,
  run: () => {
    const tab = activeTab(ws());
    if (tab) ws().setCollapsed([tab.id], !tab.collapsed);
  },
};

const COLLAPSE_OTHERS: Command = {
  id: "collapse-other-cells",
  group: "Query",
  label: "Collapse other cells",
  available: () => hasWorkspaceTab() && (shownNotebook(ws())?.cells.length ?? 0) > 1,
  run: () => {
    const active = activeId(ws());
    ws().setCollapsed(shownNotebook(ws())?.cells.filter((id) => id !== active) ?? [], true);
  },
};

const moveActiveCell = (step: -1 | 1): Command => {
  const target = () => {
    const id = activeId(ws());
    const cells = shownNotebook(ws())?.cells ?? [];
    return id && adjacentCell(cells, id, step) ? { id, index: cells.indexOf(id) + step } : undefined;
  };
  return {
    id: step < 0 ? "move-cell-up" : "move-cell-down",
    group: "Query",
    label: step < 0 ? "Move cell up" : "Move cell down",
    available: () => hasWorkspaceTab() && target() !== undefined,
    run: () => {
      const to = target();
      if (to) ws().moveCell(to.id, to.index);
    },
  };
};

export const COMMANDS: Command[] = [
  ...START_COMMANDS,
  COLLAPSE_CELL,
  COLLAPSE_OTHERS,
  {
    id: "reset-panels",
    group: "Workspace",
    label: "Reset panel sizes",
    run: () => ws().setLayout({ sidebarWidth: 240, sidebarSplit: 50, rightPanelWidth: 360, resultsHeight: 320 }),
  },
  SWITCH_NOTEBOOK,
  CLOSE_TAB,
  moveActiveCell(-1),
  moveActiveCell(1),
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
