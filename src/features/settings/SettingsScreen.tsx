import * as Dialog from "@radix-ui/react-dialog";
import { XIcon } from "lucide-react";
import { useLayoutEffect, useRef } from "react";
import type { TabId } from "../../ipc/bindings";
import { AskOverlay } from "../../shell/Overlays";
import { activeId, useWorkspace, type Screen } from "../../state/workspace";
import { DialogSurface, IconButton } from "../../ui/primitives";
import { parseQuery } from "../../ui/textMatch";
import { SearchResults, SectionBody } from "./sections";
import { SettingsSidebarBody } from "./SettingsSidebarBody";
import { filterSettings, sectionLabel } from "./settingsIndex";

type SettingsScreen = Extract<Screen, { kind: "settings" }>;

export function SettingsDialog() {
  const screen = useWorkspace((s) => s.screen);
  const closeSettings = useWorkspace((s) => s.closeSettings);
  const open = screen.kind === "settings";
  const opener = useRef<{ element: Element | null; tabId: TabId | null } | null>(null);
  useLayoutEffect(() => {
    if (open) opener.current = { element: document.activeElement, tabId: activeId(useWorkspace.getState()) };
  }, [open]);

  return (
    <Dialog.Root open={open} onOpenChange={(value) => !value && closeSettings()}>
      <Dialog.Portal>
        <DialogSurface
          data-testid="settings-dialog"
          aria-describedby={undefined}
          className="flex h-[640px] w-[960px] overflow-hidden"
          onEscapeKeyDown={(event) => event.preventDefault()}
          onKeyDown={(event) => {
            if (event.key !== "Escape" || !(event.target instanceof Node) || !event.currentTarget.contains(event.target)) return;
            event.preventDefault();
            const { screen, setSettingsQuery, closeSettings } = useWorkspace.getState();
            if (screen.kind === "settings" && screen.query) setSettingsQuery("");
            else closeSettings();
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            const workspace = useWorkspace.getState();
            if (workspace.overlay.kind !== "none" || activeId(workspace) !== opener.current?.tabId) return;
            const element = opener.current.element;
            if (element instanceof HTMLElement && element.isConnected) element.focus();
          }}
        >
          {/* Names the dialog without a heading, so the page's breadcrumb is its first heading, an h1 above the h2 sections. */}
          <Dialog.Title asChild>
            <span className="sr-only">Settings</span>
          </Dialog.Title>
          {screen.kind === "settings" && <SettingsPage screen={screen} />}
          <AskOverlay />
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export function SettingsPage({ screen }: { screen: SettingsScreen }) {
  const tokens = parseQuery(screen.query);
  const matches = tokens.length > 0 ? filterSettings(tokens) : null;
  return (
    <>
      <aside className="flex w-[200px] shrink-0 flex-col border-r border-control bg-sidebar pt-4">
        <SettingsSidebarBody section={screen.section} query={screen.query} />
      </aside>
      <div data-testid="settings-page" className="flex min-h-0 min-w-0 flex-1 flex-col">
        <header className="flex min-h-14 shrink-0 items-center gap-2 border-b border-control px-5">
          <h1 data-testid="settings-breadcrumb" className="flex min-w-0 flex-1 items-center gap-2 text-ui-14">
            <span className="truncate px-0.5 text-dim">Settings</span>
            <span aria-hidden className="text-faint">
              /
            </span>
            <span className="truncate font-medium text-fg">{matches ? "Search results" : sectionLabel(screen.section)}</span>
          </h1>
          <Dialog.Close asChild>
            <IconButton data-testid="settings-close" aria-label="Close settings" title="Close settings">
              <XIcon size={16} />
            </IconButton>
          </Dialog.Close>
        </header>
        <div className="min-h-0 flex-1 overflow-auto [scrollbar-gutter:stable_both-edges]">
          <div className="mx-auto max-w-[640px] px-8 pt-6 pb-16">
            {matches ? <SearchResults query={screen.query.trim()} matches={matches} /> : <SectionBody section={screen.section} />}
          </div>
        </div>
      </div>
    </>
  );
}
