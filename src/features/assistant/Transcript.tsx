import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ChatId, ChatState, DatabaseKind, DatabaseRef, EntrySnapshot, ReplyBlock, TurnPhase } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { groupTurns, type EndEntry, type TurnView, type UserEntry } from "../../state/transcript";
import { useWorkspace } from "../../state/workspace";
import { Button } from "../../ui/primitives";
import { agentErrorText, contextChip, type ProviderVoice } from "./assistantText";
import { sendMessage } from "./assistantActions";
import { ReplyText } from "./ReplyText";
import { ChatChoices } from "./ChatChoices";
import { ToolCard } from "./ToolCard";

const PIN_SLACK_PX = 32;

function UserMessage({ seq, entry }: { seq: number; entry: UserEntry }) {
  const chip = contextChip(entry);
  return (
    <div data-testid={`chat-user-${seq}`} className="flex flex-col items-end gap-1 pl-8">
      <p className="selectable rounded-lg bg-selected p-3 text-ui-13 leading-[22px] break-words whitespace-pre-wrap text-fg">{entry.text}</p>
      {chip && <p className="px-1 text-right text-ui-11.5 text-dim">{chip}</p>}
    </div>
  );
}

const Item = memo(function Item({
  chatId,
  snap,
  db,
  databaseKind,
}: {
  chatId: ChatId;
  snap: EntrySnapshot;
  db: DatabaseRef | null;
  databaseKind: DatabaseKind;
}) {
  const { entry, seq } = snap;
  switch (entry.kind) {
    case "Text":
      return <ReplyText testId={`chat-text-${seq}`} blocks={entry.blocks} db={db} databaseKind={databaseKind} />;
    case "Tool":
      return <ToolCard chatId={chatId} seq={seq} tool={entry.tool} status={entry.status} db={db} databaseKind={databaseKind} />;
    case "Notice":
      return <p className="text-ui-12.5 text-dim italic">{entry.text}</p>;
    case "Unreadable":
      return <p className="text-ui-12.5 text-dim italic">A newer version of L1 Data wrote this part of the chat, so it can't be shown.</p>;
    case "User":
    case "End":
      return null;
    default: {
      const unreachable: never = entry;
      return unreachable;
    }
  }
});

function EndBody({
  chatId,
  end,
  user,
  last,
  idle,
  voice,
}: {
  chatId: ChatId;
  end: EndEntry;
  user: UserEntry | null;
  last: boolean;
  idle: boolean;
  voice: ProviderVoice;
}) {
  const { outcome } = end;
  switch (outcome.kind) {
    case "Completed":
      if (!last) return null;
      return end.suggestions.length > 0 ? (
        <ChatChoices options={end.suggestions} disabled={!idle} multiple onContinue={(choices) => void sendMessage(chatId, choices.join("\n"), false)} />
      ) : null;
    case "Stopped":
      return <p className="text-ui-12.5 text-dim">Stopped</p>;
    case "Lost":
      return <p className="text-ui-12.5 text-dim">Stopped when L1 Data quit.</p>;
    case "Failed": {
      const { text, action } = agentErrorText(outcome.error, voice);
      return (
        <>
          <p className="selectable min-w-0 text-ui-12.5 break-words text-danger">{text}</p>
          {action === "settings" ? (
            <Button variant="ghost" className="h-6 px-2 text-ui-12" onClick={() => useWorkspace.getState().openSettings("providers")}>
              Open settings
            </Button>
          ) : (
            user && (
              <Button variant="ghost" className="h-6 px-2 text-ui-12" disabled={!idle} onClick={() => void sendMessage(chatId, user.text, false)}>
                Retry
              </Button>
            )
          )}
        </>
      );
    }
    default: {
      const unreachable: never = outcome;
      return unreachable;
    }
  }
}

function Turn({ chat, view, last, voice }: { chat: ChatState; view: TurnView; last: boolean; voice: ProviderVoice }) {
  const db = view.user?.entry.context?.db ?? null;
  const databaseKind = view.user?.entry.context?.databaseKind ?? "Postgres";
  const status = chat.status;
  const phase = status.kind === "Running" && status.turn === view.turn ? status.phase : null;
  const waiting = (phase === "Starting" || phase === "Working") && !view.items.some((i) => i.entry.kind === "Text");
  return (
    <div className="flex flex-col gap-3">
      {view.user && <UserMessage seq={view.user.seq} entry={view.user.entry} />}
      {view.items.map((snap) => (
        <Item key={snap.seq} chatId={chat.id} snap={snap} db={db} databaseKind={databaseKind} />
      ))}
      {waiting && <p className="animate-pulse text-ui-12.5 text-dim">{phase === "Starting" ? `Starting ${voice.name}…` : "Thinking…"}</p>}
      {view.end && (
        <div data-testid={`chat-end-${view.end.seq}`} data-outcome={view.end.entry.outcome.kind} className="flex flex-col gap-2 empty:hidden">
          <EndBody chatId={chat.id} end={view.end.entry} user={view.user?.entry ?? null} last={last} idle={status.kind === "Idle"} voice={voice} />
        </div>
      )}
    </div>
  );
}

/** A reply as a screen reader should hear it: the words without the markdown marks around them. */
function spoken(blocks: readonly ReplyBlock[]): string {
  return blocks
    .map((b) => (b.kind === "Sql" ? b.sql : b.text.replace(/^\s*(?:#+|>)\s*/gm, "").replace(/[*`|]|-{3,}/g, " ")))
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}

function replyText(view: TurnView): string {
  return view.items
    .map(({ entry }) => (entry.kind === "Text" ? spoken(entry.blocks) : ""))
    .join(" ")
    .trim();
}

function approvalText(view: TurnView, voice: ProviderVoice): string {
  const waiting = view.items.find(({ entry }) => entry.kind === "Tool" && entry.status.kind === "AwaitingApproval")?.entry;
  if (waiting?.kind !== "Tool" || waiting.tool.kind !== "RunQuery") return `${voice.name} asks to call a tool. Run or deny it in the chat.`;
  return `${voice.name} asks to run a query${waiting.tool.purpose ? `: ${waiting.tool.purpose}` : ""}. Run or deny it in the chat.`;
}

type Announcement = { polite: string; assertive: string };

/**
 * What the transcript says to a screen reader. Streamed tokens would be read a fragment at a time, so the transcript
 * itself is silent and this speaks whole moments instead: a turn starting, a finished reply, a question that waits on
 * the user, and a failure. It speaks only of changes while the chat is shown, never of the history it opens with.
 */
function useAnnouncement(chat: ChatState, last: TurnView | undefined, voice: ProviderVoice): Announcement {
  const [said, setSaid] = useState<Announcement>({ polite: "", assertive: "" });
  const status = chat.status;
  const phase: TurnPhase | null = status.kind === "Running" && status.turn === last?.turn ? status.phase : null;
  const endSeq = last?.end?.seq ?? null;
  const seen = useRef<{ chat: ChatId; endSeq: number | null; phase: TurnPhase | null } | null>(null);
  useEffect(() => {
    const before = seen.current;
    seen.current = { chat: chat.id, endSeq, phase };
    if (!last || !before || before.chat !== chat.id) return;
    let next: Partial<Announcement> | null = null;
    if (last.end && endSeq !== before.endSeq) {
      const { outcome } = last.end.entry;
      if (outcome.kind === "Completed") next = { polite: `${voice.name} replied. ${replyText(last)}`, assertive: "" };
      else if (outcome.kind === "Failed") next = { polite: "", assertive: `${voice.name} failed. ${agentErrorText(outcome.error, voice).text}` };
      else if (outcome.kind === "Stopped") next = { polite: "Stopped", assertive: "" };
    } else if (phase !== before.phase) {
      if (phase === "AwaitingApproval") next = { assertive: approvalText(last, voice) };
      else if (before.phase === null && phase !== null) next = { polite: phase === "Starting" ? `Starting ${voice.name}…` : "Thinking…", assertive: "" };
      // An answered question is cleared, so the same question asked again is heard again.
      else if (before.phase === "AwaitingApproval") next = { assertive: "" };
    }
    if (next) {
      const change = next;
      // oxlint-disable-next-line react/set-state-in-effect -- an announcement is a change seen after the render that drew it
      setSaid((s) => ({ ...s, ...change }));
    }
  }, [chat.id, endSeq, phase, last, voice]);
  return said;
}

export function Transcript({ chat }: { chat: ChatState }) {
  const entries = useLive((s) => s.transcripts[chat.id]);
  const turns = useMemo(() => groupTurns(entries), [entries]);
  const name = useWorkspace((s) => s.settings.providers.find((p) => p.provider === chat.options.provider)?.displayName);
  const signInCommand = useLive((s) => s.providers[chat.options.provider]?.info.signInCommand);
  const voice = useMemo(() => ({ name: name ?? chat.options.provider, signInCommand: signInCommand ?? "" }), [name, chat.options.provider, signInCommand]);
  const said = useAnnouncement(chat, turns.at(-1), voice);

  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  useEffect(() => {
    const el = scroller.current;
    const inner = content.current;
    if (!el || !inner) return;
    const follow = new ResizeObserver(() => {
      if (pinned.current) el.scrollTop = el.scrollHeight;
    });
    follow.observe(inner);
    return () => follow.disconnect();
  }, []);
  const lastTurn = turns.at(-1)?.turn;
  useLayoutEffect(() => {
    pinned.current = true;
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lastTurn]);

  return (
    <div
      ref={scroller}
      onScroll={(e) => {
        const el = e.currentTarget;
        pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < PIN_SLACK_PX;
      }}
      className="min-h-0 flex-1 overflow-y-auto"
    >
      <div data-testid="assistant-announcement" role="status" className="sr-only">
        {said.polite}
      </div>
      <div data-testid="assistant-alert" role="alert" className="sr-only">
        {said.assertive}
      </div>
      {/* A log, but a quiet one: streamed tokens would be read a fragment at a time. The regions above speak for it. */}
      <div ref={content} role="log" aria-live="off" aria-label="Chat transcript" className="flex flex-col gap-6 px-4 pt-2 pb-6">
        {turns.length === 0 ? (
          <p className="pt-10 text-center text-ui-13 text-dim">Ask for help with your database.</p>
        ) : (
          turns.map((t, i) => <Turn key={t.turn} chat={chat} view={t} last={i === turns.length - 1} voice={voice} />)
        )}
      </div>
    </div>
  );
}
