import { PanelLeftDashedIcon, PanelLeftIcon, PanelRightDashedIcon, PanelRightIcon } from "lucide-react";
import { useWorkspace } from "../state/workspace";
import { IconButton } from "../ui/primitives";
import { WindowControls } from "./WindowChrome";
import type { HostedWindow } from "./hostedWindow";

export function windowEdgeInset(hosted: HostedWindow | null): string {
  return hosted?.chrome === "custom" ? "pr-2" : "pr-3";
}

// The macOS traffic lights (traffic_light_position 18,30 in src-tauri) end at x=77; 86px leaves the same gap they leave between each other.
export function windowLeadingInset(hosted: HostedWindow | null): string {
  return hosted?.chrome === "native" ? "pl-[86px]" : "pl-3";
}

// A header under a closed panel's toggle clears it with padding rather than a spacer element, so the clearance
// transitions alongside the sliding column instead of appearing in one frame. Leading: the inset and the size-7
// toggle, with no gap because .workspace-context zeroes the leading group's. Trailing: the inset, a gap-2, and the spacer.
export function windowLeadingClearance(hosted: HostedWindow | null): string {
  return hosted?.chrome === "native" ? "pl-[calc(86px+var(--spacing)*7)]" : "pl-10";
}

export function windowEdgeClearance(hosted: HostedWindow | null): string {
  return hosted?.chrome === "custom" ? "pr-[calc(var(--spacing)*38+1px)]" : "pr-12";
}

export const WINDOW_HEADER_HEIGHT = "h-[56px]";

const PANEL_ICONS = {
  left: { hidden: PanelLeftDashedIcon, shown: PanelLeftIcon },
  right: { hidden: PanelRightDashedIcon, shown: PanelRightIcon },
};

function PanelIcon({ side, shown }: { side: keyof typeof PANEL_ICONS; shown: boolean }) {
  const Icon = PANEL_ICONS[side][shown ? "shown" : "hidden"];
  return <Icon size={16} />;
}

export function SidebarToggle() {
  const sidebar = useWorkspace((s) => s.layout.sidebar);
  const setLayout = useWorkspace((s) => s.setLayout);
  return (
    <IconButton
      data-testid="toggle-sidebar"
      aria-label="Toggle sidebar"
      aria-expanded={sidebar}
      className="shrink-0"
      onClick={() => setLayout({ sidebar: !sidebar })}
    >
      <PanelIcon side="left" shown={sidebar} />
    </IconButton>
  );
}

export function WindowEdgeActionsSpacer({ hosted }: { hosted: HostedWindow | null }) {
  return <div aria-hidden className={`shrink-0 ${hosted?.chrome === "custom" ? "w-[calc(var(--spacing)*34+1px)]" : "w-7"}`} />;
}

export function WindowEdgeActions({ hosted }: { hosted: HostedWindow | null }) {
  const rightPanel = useWorkspace((s) => s.layout.rightPanel);
  const setLayout = useWorkspace((s) => s.setLayout);
  return (
    <div className="flex items-center gap-2">
      <IconButton
        data-testid="toggle-right-panel"
        aria-label="Toggle right panel"
        aria-expanded={rightPanel !== null}
        onClick={() => setLayout({ rightPanel: rightPanel ? null : "Assistant" })}
      >
        <PanelIcon side="right" shown={rightPanel !== null} />
      </IconButton>
      {hosted?.chrome === "custom" && (
        <>
          <div className="mx-1 h-5 w-px bg-border" />
          <WindowControls hosted={hosted} />
        </>
      )}
    </div>
  );
}
