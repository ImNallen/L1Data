import * as Menu from "@radix-ui/react-dropdown-menu";
import { ArrowUpIcon, BrainIcon, ChevronDownIcon, LockIcon, LockOpenIcon, PaperclipIcon, SquareIcon, TableIcon, TriangleAlertIcon, XIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { commands, type ChatOptions, type ChatState, type ModelOption, type Permission } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { databaseKindFor } from "../../state/databaseKind";
import { tabTarget, useWorkspace } from "../../state/workspace";
import { cx, IconButton, MenuContent, MenuItem, MenuLabel } from "../../ui/primitives";
import { pluralRows } from "../results/format";
import { configureChat, effortFor, gridResult, sendMessage, stopTurn } from "./assistantActions";
import { providerBanner, type ProviderBanner } from "./assistantText";
import { PROVIDER_ICONS } from "./providerIcons";

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

const Divider = () => <span aria-hidden className="mx-0.5 h-4 w-px shrink-0 bg-border" />;

function Pickers({ chat, models }: { chat: ChatState; models: ModelOption[] }) {
  const full = useWorkspace((s) => s.settings.assistantAccess === "Full");
  const { options } = chat;
  const Icon = PROVIDER_ICONS[options.provider];
  const model = models.find((m) => m.id === options.model);
  const efforts = model?.efforts ?? [];
  const configure = (patch: Partial<ChatOptions>) => void configureChat(chat.id, { ...options, ...patch });
  const permission = PERMISSIONS[options.permission];
  return (
    <div className="flex min-w-0 items-center">
      <Picker testId="assistant-model" label={model?.label ?? options.model} icon={<Icon size={13} />}>
        {models.length === 0 && <MenuLabel>Models appear once the provider is checked.</MenuLabel>}
        {models.map((m) => (
          <MenuItem key={m.id} checked={m.id === options.model} onSelect={() => configure({ model: m.id, effort: effortFor(m, options.effort) })}>
            {m.label}
            {m.recommended && <span className="ml-auto pl-4 text-ui-11.5 text-dim">Recommended</span>}
          </MenuItem>
        ))}
      </Picker>
      {efforts.length > 0 && (
        <>
          <Divider />
          <Picker
            testId="assistant-effort"
            label={efforts.find((e) => e.id === options.effort)?.label ?? "Effort"}
            icon={<BrainIcon size={13} className="shrink-0" />}
          >
            {efforts.map((e) => (
              <MenuItem key={e.id} checked={e.id === options.effort} onSelect={() => configure({ effort: e.id })}>
                {e.label}
              </MenuItem>
            ))}
          </Picker>
        </>
      )}
      {full && <Divider />}
      {full && (
        <Picker testId="assistant-permission" label={permission.label} icon={<permission.Icon size={13} className="shrink-0" />}>
          {(Object.keys(PERMISSIONS) as Permission[]).map((p) => (
            <MenuItem key={p} checked={p === options.permission} onSelect={() => configure({ permission: p })}>
              {PERMISSIONS[p].label}
              <span className="ml-auto pl-4 text-ui-11.5 text-dim">{PERMISSIONS[p].hint}</span>
            </MenuItem>
          ))}
        </Picker>
      )}
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
      {sqlServer && (
        <p data-testid="assistant-sqlserver-limit" className="mb-2 text-ui-12 leading-relaxed text-dim">
          The assistant can inspect SQL Server schema and suggest SQL. Its run_query tool is unavailable for SQL Server. Open suggested SQL in a tab to run it.
        </p>
      )}
      <div data-testid="assistant-access" className="mb-2 text-ui-12 leading-relaxed text-dim">
        <p className="mb-1">
          {full
            ? "Editor SQL and errors are sent. Chat text is sent as written."
            : "Schema structure and your chat text are sent, including anything you paste."}
        </p>
        <div className="flex min-w-0 items-center gap-2">
          <span data-testid="assistant-target" title={targetLabel} className="min-w-0 flex-1 truncate text-muted">
            {targetLabel}
          </span>
          <button
            type="button"
            className="shrink-0 text-muted underline-offset-2 hover:underline"
            onClick={() => useWorkspace.getState().openSettings("general")}
          >
            {full ? "Full access" : "Schema only"}
          </button>
        </div>
      </div>
      <div className="rounded-xl border border-control bg-raised focus-within:border-focus">
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
          placeholder={full ? "Ask about your data, send follow-ups, or attach a result" : "Ask for SQL help using your schema"}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing) return;
            e.preventDefault();
            void send();
          }}
          className="block w-full resize-none bg-transparent px-3 pt-2.5 pb-1 text-ui-13 leading-relaxed text-fg outline-none placeholder:text-dim"
        />
        <div className="flex items-center gap-1 px-1.5 pb-1.5">
          {chat && <Pickers chat={chat} models={models} />}
          <div className="flex-1" />
          {full && (
            <IconButton
              data-testid="assistant-attach"
              aria-label="Attach the result this tab shows"
              aria-pressed={attaching}
              title={shown ? "Attach the result this tab shows" : "Run a query to attach its result"}
              disabled={!shown}
              onClick={() => setAttach((a) => !a)}
              className={cx(attaching && "bg-pill text-fg")}
            >
              <PaperclipIcon size={15} />
            </IconButton>
          )}
          {running ? (
            <button
              type="button"
              data-testid="assistant-stop"
              aria-label="Stop"
              title="Stop"
              onClick={() => stopTurn(chat.id)}
              className="grid size-7 place-items-center rounded-full bg-muted text-bg transition-colors hover:bg-fg"
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
              className="grid size-7 place-items-center rounded-full bg-muted text-bg transition-colors hover:bg-fg disabled:pointer-events-none disabled:opacity-40"
            >
              <ArrowUpIcon size={15} strokeWidth={2.25} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
