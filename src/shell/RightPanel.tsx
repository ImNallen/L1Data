import { PlusIcon } from "lucide-react";
import { newChat } from "../features/assistant/assistantActions";
import { ChatHistory } from "../features/assistant/ChatHistory";
import { cx, IconButton } from "../ui/primitives";
import type { HostedWindow } from "./hostedWindow";
import { AssistantPanel } from "./lazySurfaces";
import { WINDOW_HEADER_HEIGHT, WindowEdgeActions, windowEdgeInset } from "./WindowEdgeActions";

export function RightPanel({ hosted }: { hosted: HostedWindow | null }) {
  return (
    <aside data-testid="right-panel" aria-labelledby="right-panel-title" className="assistant-surface flex h-full flex-col">
      <div data-tauri-drag-region="deep" className={cx("flex shrink-0 items-center gap-1 pl-3", WINDOW_HEADER_HEIGHT, windowEdgeInset(hosted))}>
        <h2 id="right-panel-title" className="px-1 text-ui-14 font-semibold text-fg">
          Assistant
        </h2>
        <div className="flex-1" />
        <IconButton data-testid="assistant-new" aria-label="New chat" title="New chat" onClick={() => void newChat()}>
          <PlusIcon size={16} />
        </IconButton>
        <ChatHistory />
        <WindowEdgeActions hosted={hosted} />
      </div>
      <AssistantPanel />
    </aside>
  );
}
