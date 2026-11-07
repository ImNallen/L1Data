import * as Menu from "@radix-ui/react-dropdown-menu";
import { ArrowUpIcon, ChevronDownIcon, LockIcon, LockOpenIcon, PlusIcon, DatabaseIcon, SquareIcon, TableIcon, TriangleAlertIcon, XIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { commands, type ChatOptions, type ChatState, type ModelOption, type Permission } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { useLive } from "../../state/live";
import { databaseKindFor } from "../../state/databaseKind";
import { tabTarget, useWorkspace } from "../../state/workspace";
import { IconButton, MenuContent, MenuItem, MenuLabel } from "../../ui/primitives";
import { pluralRows } from "../results/format";
import { configureChat, effortFor, gridResult, sendMessage, stopTurn } from "./assistantActions";
import { providerBanner, type ProviderBanner } from "./assistantText";

const MAX_INPUT_PX = 200;

const PERMISSIONS = {
  AskFirst: { label: "Ask first", hint: "Every query waits for Run or Deny", Icon: LockIcon },
  Auto: { label: "Auto", hint: "Read-only queries run without asking", Icon: LockOpenIcon },
} satisfies Record<Permission, unknown>;

function Picker({ testId, label, icon, disabled, children }: { testId: string; label: string; icon: ReactNode; disabled?: boolean; children: ReactNode }) {
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger
        data-testid={testId}
        disabled={disabled}
        className="inline-flex h-7 min-w-0 items-center gap-1.5 rounded-md px-1.5 text-ui-12.5 text-muted hover:bg-hover hover:text-fg disabled:pointer-events-none disabled:opacity-40 data-[state=open]:bg-hover data-[state=open]:text-fg"
      >
        {icon}
        <span className="truncate">{label}</span>
        <ChevronDownIcon size={12} className="shrink-0 text-dim" />
      </Menu.Trigger>
      <MenuContent>{children}</MenuContent>
    </Menu.Root>
  );
}

function Pickers({ chat, models }: { chat: ChatState; models: ModelOption[] }) {
  const { options } = chat;
  const model = models.find((m) => m.id === options.model);
  const configure = (patch: Partial<ChatOptions>) => void configureChat(chat.id, { ...options, ...patch });
  return (
    <div className="flex min-w-0 items-center">
      <Picker testId="assistant-model" label={model?.label ?? options.model} icon={null}>
        {models.length === 0 && <MenuLabel>Models appear once the provider is checked.</MenuLabel>}
        {models.map((m) => (
          <MenuItem key={m.id} checked={m.id === options.model} onSelect={() => configure({ model: m.id, effort: effortFor(m, options.effort) })}>
            {m.label}
            {m.recommended && <span className="ml-auto pl-4 text-ui-11.5 text-dim">Recommended</span>}
          </MenuItem>
        ))}
      </Picker>
    </div>
  );
}

function bannerMessage(banner: ProviderBanner): ReactNode {
  switch (banner.kind) {
    case "Off":
      return `${banner.name} is turned off. Turn it on to chat.`;
    case "NotFound":
      return `${banner.name} CLI not found at ${banner.binary}.`;
    case "SignedOut":
      return (
        <>
          {banner.name} is signed out. Run <code className="rounded-[4px] bg-pill px-1 font-mono text-code-sm text-fg">{banner.command}</code> in a terminal,
          then{" "}
          <button type="button" className="text-fg underline underline-offset-2 hover:text-fg/80" onClick={() => void commands.providerRefresh()}>
            refresh
          </button>
          .
        </>
      );
    case "Broken":
      return `${banner.name} is not working. ${banner.message}`;
    default: {
      const unreachable: never = banner;
      return unreachable;
    }
  }
}

function Banner({ banner }: { banner: ProviderBanner }) {
  return (
    <div
      data-testid="assistant-provider-banner"
      className="mb-2 flex items-start gap-2 rounded-lg border border-control px-3 py-2 text-ui-12 leading-relaxed text-muted"
    >
      <TriangleAlertIcon size={13} className="mt-[3px] shrink-0 text-amber" />
      <p className="min-w-0 flex-1">{bannerMessage(banner)}</p>
      <button type="button" className="shrink-0 text-fg underline-offset-2 hover:underline" onClick={() => useWorkspace.getState().openSettings("providers")}>
        Open settings
      </button>
    </div>
  );
}

export function Composer({ chat }: { chat: ChatState | undefined }) {
  const [text, setText] = useState("");
  const [attach, setAttach] = useState(false);
  const [sending, setSending] = useState(false);
  const input = useRef<HTMLTextAreaElement>(null);

  const active = useWorkspace((s) => s.active);
  const full = useWorkspace((s) => s.settings.assistantAccess === "Full");
  const targetLabel = useWorkspace((s) => {
    const db = tabTarget(s.active ? s.tabs[s.active] : undefined);
    if (!db) return "No database selected";
    return `${s.configs[db.connectionId]?.name ?? "Unknown connection"} / ${db.database}`;
  });
  const db = useWorkspace((s) => tabTarget(s.active ? s.tabs[s.active] : undefined));
  const schema = useWorkspace((s) => (s.active ? s.tabs[s.active]?.console.schema : null));
  const catalog = useCatalog((s) => viewFor(s.views, db)?.catalog);
  const contextLabel = db ? `${db.database} / ${schema ?? catalog?.defaultSchema ?? "default"}` : targetLabel;
  const tabTitle = useWorkspace((s) => (s.active ? s.tabs[s.active]?.title : undefined));
  const sqlServer = useWorkspace((s) => databaseKindFor(tabTarget(s.active ? s.tabs[s.active] : undefined), s.configs) === "SqlServer");
  const picked = useWorkspace((s) => (s.active ? s.activeResult[s.active] : undefined));
  const run = useLive((s) => (active ? s.consoles[active]?.run : undefined));
  const hasOutput = useLive((s) => (active ? (s.output[active]?.entries.length ?? 0) : 0) > 0);
  const shown = gridResult(run, picked, hasOutput);
  const attaching = full && attach && shown !== null;
  useEffect(
    () =>
      useWorkspace.subscribe((s, previous) => {
        if (s.settings.assistantAccess !== previous.settings.assistantAccess) setAttach(false);
      }),
    [],
  );

  const provider = chat?.options.provider;
  const config = useWorkspace((s) => s.settings.providers.find((p) => p.provider === provider));
  const state = useLive((s) => (provider ? s.providers[provider] : undefined));
  const banner = chat ? providerBanner(config, state) : null;
  const models = state?.outcome?.kind === "Ready" ? state.outcome.models : [];
  const efforts = models.find((m) => m.id === chat?.options.model)?.efforts ?? [];
  const configure = (patch: Partial<ChatOptions>) => {
    if (chat) void configureChat(chat.id, { ...chat.options, ...patch });
  };
  const running = chat?.status.kind === "Running";
  const canSend = chat !== undefined && !running && !sending && banner?.kind !== "Off" && text.trim() !== "";

  useLayoutEffect(() => {
    const el = input.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, MAX_INPUT_PX)}px`;
  }, [text]);

  const send = async () => {
    if (!canSend) return;
    setSending(true);
    const ok = await sendMessage(chat.id, text.trim(), attaching);
    setSending(false);
    if (ok) {
      setText("");
      setAttach(false);
    }
  };

  return (
    <div className="shrink-0 px-3 pb-3">
      {banner && <Banner banner={banner} />}
      <div data-testid="assistant-access" className="sr-only">
        {full ? "Editor SQL and errors are sent. Chat text is sent as written." : "Schema structure and your chat text are sent, including anything you paste."}
      </div>
      <div className="chat-composer">
        {attaching && (
          <div className="flex px-3 pt-2.5">
            <span
              data-testid="assistant-attachment"
              className="inline-flex min-w-0 items-center gap-1.5 rounded-md bg-pill py-0.5 pr-0.5 pl-2 text-ui-12 text-muted"
            >
              <TableIcon size={12} className="shrink-0" />
              <span className="truncate">
                {tabTitle} · {pluralRows(shown.meta.rowCount)}
              </span>
              <button
                type="button"
                aria-label="Remove attachment"
                onClick={() => setAttach(false)}
                className="grid size-4 shrink-0 place-items-center rounded hover:bg-hover hover:text-fg"
              >
                <XIcon size={11} />
              </button>
            </span>
          </div>
        )}
        <textarea
          ref={input}
          data-testid="assistant-input"
          aria-label="Message the assistant"
          rows={2}
          value={text}
          placeholder="Message assistant…"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing) return;
            e.preventDefault();
            void send();
          }}
          className="block w-full resize-none bg-transparent text-fg outline-none placeholder:text-dim"
        />
        <div className="mt-2 flex items-center gap-1">
          <Menu.Root modal={false}>
            <Menu.Trigger asChild>
              <IconButton data-testid="assistant-options" aria-label="Assistant options" title="Add context and options">
                <PlusIcon size={16} />
              </IconButton>
            </Menu.Trigger>
            <MenuContent>
              {full && (
                <MenuItem testId="assistant-attach" disabled={!shown} onSelect={() => setAttach((a) => !a)}>
                  <TableIcon size={13} />
                  {attaching ? "Remove result attachment" : "Attach current result"}
                </MenuItem>
              )}
              {efforts.length > 0 && <MenuLabel>Reasoning effort</MenuLabel>}
              {efforts.map((effort) => (
                <MenuItem key={effort.id} checked={chat?.options.effort === effort.id} onSelect={() => configure({ effort: effort.id })}>
                  {effort.label}
                </MenuItem>
              ))}
              {full && <MenuLabel>Query permissions</MenuLabel>}
              {full &&
                (["AskFirst", "Auto"] as const).map((permission) => (
                  <MenuItem
                    key={permission}
                    testId={`assistant-permission-${permission}`}
                    checked={chat?.options.permission === permission}
                    onSelect={() => configure({ permission })}
                  >
                    {PERMISSIONS[permission].label}
                    <span className="ml-auto pl-3 text-ui-11 text-dim">{PERMISSIONS[permission].hint}</span>
                  </MenuItem>
                ))}
              <MenuLabel>Sharing</MenuLabel>
              <MenuItem onSelect={() => useWorkspace.getState().openSettings("general")}>{full ? "Full access" : "Schema only"}</MenuItem>
              <MenuLabel>{full ? "Editor SQL, errors and chat text are shared." : "Schema and chat text, including pasted text."}</MenuLabel>
            </MenuContent>
          </Menu.Root>
          {chat && <Pickers chat={chat} models={models} />}
          <div className="flex-1" />
          {running ? (
            <button
              type="button"
              data-testid="assistant-stop"
              aria-label="Stop"
              title="Stop"
              onClick={() => stopTurn(chat.id)}
              className="grid size-7 place-items-center rounded-full bg-fg text-bg transition-colors hover:opacity-85"
            >
              <SquareIcon fill="currentColor" size={12} />
            </button>
          ) : (
            <button
              type="button"
              data-testid="assistant-send"
              aria-label="Send"
              title="Send"
              disabled={!canSend}
              onClick={() => void send()}
              className="grid size-7 place-items-center rounded-full bg-fg text-bg transition-colors hover:opacity-85 disabled:pointer-events-none disabled:opacity-40"
            >
              <ArrowUpIcon size={15} strokeWidth={2.25} />
            </button>
          )}
        </div>
      </div>
      {db || attaching ? (
        <div className="chat-composer-details">
          <DatabaseIcon size={12} className="shrink-0" />
          <span data-testid="assistant-target" title={targetLabel} className="min-w-0 flex-1 truncate">
            {contextLabel}
          </span>
          <button
            type="button"
            title={full ? "Editor SQL, errors and chat text are shared" : "Schema and chat text are shared, including anything you paste"}
            onClick={() => useWorkspace.getState().openSettings("general")}
            className="shrink-0 hover:text-fg"
          >
            {full ? "Full access" : "Schema only"}
          </button>
        </div>
      ) : (
        <span data-testid="assistant-target" title={targetLabel} className="sr-only">
          {targetLabel}
        </span>
      )}
      {sqlServer && (
        <p data-testid="assistant-sqlserver-limit" className="sr-only">
          SQL Server assistant tools inspect schema only; run suggested SQL manually in chat or a notebook cell.
        </p>
      )}
    </div>
  );
}
