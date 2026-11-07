import { ChevronDownIcon } from "lucide-react";
import { useEffect } from "react";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { newChat, openChat } from "./assistantActions";
import { Composer } from "./Composer";
import { Transcript } from "./Transcript";

export function AssistantPanel() {
  const chatId = useWorkspace((s) => s.layout.assistantChat);
  const chat = useLive((s) => (chatId ? s.chats[chatId] : undefined));
  const opened = useLive((s) => (chatId ? s.transcripts[chatId] !== undefined : false));
  const deleted = chat?.status.kind === "Deleted";
  useEffect(() => {
    if (chatId === null || deleted) void newChat();
    else if (!opened) void openChat(chatId);
  }, [chatId, deleted, opened]);
  return (
    <div data-testid="assistant-panel" className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-2 px-4 text-ui-12 font-medium">
        <span className="min-w-0 truncate">{chat?.title || "New conversation"}</span>
        <ChevronDownIcon size={10} className="text-dim" />
      </div>
      {chat && !deleted ? <Transcript key={chat.id} chat={chat} /> : <div className="flex-1" />}
      <Composer chat={deleted ? undefined : chat} />
    </div>
  );
}
