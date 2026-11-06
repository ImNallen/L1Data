import * as Dialog from "@radix-ui/react-dialog";
import { CheckIcon, ChevronsUpDownIcon, CircleAlertIcon, EyeIcon, EyeOffIcon, KeyIcon, LockIcon, PlusIcon, XIcon } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import {
  commands,
  type ConnectionConfig,
  type ConnectionDraft,
  type ConnectionId,
  type Environment,
  type SqlServerAuth,
  type SslMode,
} from "../../ipc/bindings";
import { connectErrorText } from "../../ipc/errors";
import { DATABASE_LABEL } from "../../state/databaseKind";
import { useLive } from "../../state/live";
import { connectionTone } from "../../state/status";
import { useWorkspace } from "../../state/workspace";
import {
  Button,
  CONNECTION_TONE_LABEL,
  cx,
  DatabaseBadge,
  DialogSurface,
  IconButton,
  inputClass,
  Modal,
  Spinner,
  StatusDot,
  Switch,
} from "../../ui/primitives";
import { connect, unknownHostKey, type UnknownHostKey } from "./connectionActions";
import { CONNECTION_COLORS, ConnectionColorDot, connectionColorId, type ConnectionColorId } from "./connectionColors";
import {
  entraNeedsVerifiedTls,
  initialFields,
  initialPassword,
  initialSshSecret,
  isDirty,
  parsePort,
  signsInWithEntra,
  sshValid,
  takesPassword,
  toPassword,
  toSqlServerAuth,
  toSshDraft,
  toUser,
  type Fields,
  type PasswordEdit,
  type SshAuthKind,
} from "./connectionForm";

/** Each hint says what the engine's `tls()` does for the mode. */
const SSL_MODES: { value: SslMode; label: string; hint: string }[] = [
  { value: "Disable", label: "Disable", hint: "Never uses TLS." },
  { value: "Prefer", label: "Prefer", hint: "Uses TLS when the server offers it, without checking its certificate." },
  { value: "Require", label: "Require", hint: "Always uses TLS, without checking its certificate." },
  { value: "VerifyCa", label: "Verify CA", hint: "Always uses TLS and checks the certificate chain, but not the host name." },
  { value: "VerifyFull", label: "Verify full", hint: "Always uses TLS and checks the certificate chain and the host name." },
];

const ENVIRONMENTS: { value: Environment | null; label: string }[] = [
  { value: null, label: "Not set" },
  { value: "Local", label: "Develop" },
  { value: "Staging", label: "Staging" },
  { value: "Production", label: "Production" },
];

const SQL_SERVER_AUTHS: { value: SqlServerAuth; label: string; hint: ReactNode }[] = [
  { value: "SqlLogin", label: "SQL login", hint: "A SQL Server login and password. Windows authentication is not supported." },
  {
    value: "EntraAzureCli",
    label: "Microsoft Entra ID (Azure CLI)",
    hint: (
      <>
        Signs in as the account of <code className="font-mono">az login</code>. No password is saved.
      </>
    ),
  },
  {
    value: "Unknown",
    label: "Set by a newer version",
    hint: "A newer version of L1 Data saved this sign-in method. Update L1 Data, or choose another method.",
  },
];

const SSH_AUTHS: { value: SshAuthKind; label: string }[] = [
  { value: "Password", label: "Password" },
  { value: "Key", label: "Key file" },
  { value: "Agent", label: "SSH agent" },
];

type Probe =
  | { kind: "idle" }
  | { kind: "testing" }
  | { kind: "ok"; version: string }
  | { kind: "error"; message: string }
  | { kind: "hostKey"; key: UnknownHostKey };

function serverText(c: ConnectionConfig): string {
  const server = `${c.host}:${c.port}/${c.database}`;
  return c.ssh ? `${server} via ${c.ssh.host}:${c.ssh.port}` : server;
}

function ListRow({
  testId,
  selected,
  mark,
  name,
  detail,
  trailing,
  onClick,
}: {
  testId: string;
  selected: boolean;
  mark: ReactNode;
  name: ReactNode;
  detail: string;
  trailing?: ReactNode;
  onClick?: () => void;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      aria-current={selected || undefined}
      onClick={onClick}
      className={cx("flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left", selected ? "bg-selected" : "hover:bg-hover")}
    >
      <span className="grid w-2.5 shrink-0 place-items-center">{mark}</span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-ui-13 text-fg">{name}</span>
        <span title={detail} className="block truncate font-mono text-ui-11 text-dim">
          {detail}
        </span>
      </span>
      {trailing}
    </button>
  );
}

function HollowMark({ dashed }: { dashed?: boolean }) {
  return <span className={cx("size-2 rounded-full border border-faint", dashed && "border-dashed")} />;
}

function ConnectionList({ selected, onSelect }: { selected: ConnectionId | null; onSelect: (id: ConnectionId | null) => void }) {
  const configs = useWorkspace((s) => s.configs);
  const connections = useLive((s) => s.connections);
  const list = Object.values(configs).sort((a, b) => a.name.localeCompare(b.name));
  return (
    <aside className="flex w-[210px] shrink-0 flex-col border-r border-border bg-sidebar">
      <div className="flex h-12 shrink-0 items-center justify-between pr-2 pl-4">
        <span className="text-ui-13.5 font-semibold text-fg">Connections</span>
        <Button data-testid="conn-new" variant="ghost" className="h-7 gap-1 px-2" onClick={() => onSelect(null)}>
          <PlusIcon size={14} /> New
        </Button>
      </div>
      <div className="min-h-0 flex-1 space-y-0.5 overflow-y-auto px-2 pb-2">
        {selected === null && (
          <ListRow testId="conn-list-new" selected mark={<HollowMark dashed />} name={<span className="italic">New connection</span>} detail="Not saved yet" />
        )}
        {list.map((c) => {
          const tone = connectionTone(connections[c.id]);
          return (
            <ListRow
              key={c.id}
              testId={`conn-list-${c.name}`}
              selected={c.id === selected}
              mark={connectionColorId(c.color) ? <ConnectionColorDot color={c.color} /> : <HollowMark />}
              name={c.name}
              detail={serverText(c)}
              trailing={tone !== "off" && <StatusDot tone={tone} label={CONNECTION_TONE_LABEL[tone]} />}
              onClick={() => onSelect(c.id)}
            />
          );
        })}
      </div>
      <div className="flex shrink-0 gap-2 border-t border-border px-4 py-3 text-ui-11.5 leading-4 text-dim">
        <LockIcon size={14} className="mt-px shrink-0" />
        <span>Passwords are encrypted with a key kept in the system keychain.</span>
      </div>
    </aside>
  );
}

/** The id of the hint a `FormRow` draws for the field `htmlFor` names. The field points `aria-describedby` at it. */
const hintOf = (htmlFor: string) => `${htmlFor}-hint`;

/** `live` reads the hint aloud when it changes, for a hint that turns into the field's error. */
function FormRow({
  label,
  htmlFor,
  labelId,
  hint,
  live,
  children,
}: {
  label: string;
  htmlFor?: string;
  labelId?: string;
  hint?: ReactNode;
  live?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="grid grid-cols-[110px_minmax(0,1fr)] gap-x-4 border-b border-border py-3.5 last:border-b-0">
      <label id={labelId} htmlFor={htmlFor} className="pt-1.5 text-ui-13 text-muted">
        {label}
      </label>
      <div>
        {children}
        {hint && (
          <div id={htmlFor && hintOf(htmlFor)} aria-live={live ? "polite" : undefined} className="mt-1.5 text-ui-12 text-dim">
            {hint}
          </div>
        )}
      </div>
    </div>
  );
}

function SecretInput({
  testId,
  label,
  value,
  placeholder,
  onChange,
}: {
  testId: string;
  label: string;
  value: string;
  placeholder: string;
  onChange: (value: string) => void;
}) {
  const [reveal, setReveal] = useState(false);
  const noun = label.toLowerCase();
  return (
    <div className="relative w-full">
      <input
        data-testid={testId}
        aria-label={label}
        type={reveal ? "text" : "password"}
        className={cx(inputClass, "pr-8")}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
      <button
        type="button"
        data-testid={`${testId}-reveal`}
        aria-label={reveal ? `Hide ${noun}` : `Show ${noun}`}
        aria-pressed={reveal}
        onClick={() => setReveal(!reveal)}
        className="absolute top-1/2 right-1 grid size-6 -translate-y-1/2 place-items-center rounded-md text-dim hover:bg-hover hover:text-fg"
      >
        {reveal ? <EyeOffIcon size={14} /> : <EyeIcon size={14} />}
      </button>
    </div>
  );
}

function ClearSecretButton({ testId, noun, cleared, onToggle }: { testId: string; noun: string; cleared: boolean; onToggle: () => void }) {
  return (
    <button type="button" data-testid={testId} onClick={onToggle} className="hover:text-fg">
      {cleared ? `Keep the saved ${noun}` : `Remove the saved ${noun}`}
    </button>
  );
}

function secretPlaceholder(edit: PasswordEdit, saved: boolean, noun: string, empty: string): string {
  if (edit.kind === "keep") return `Saved ${noun}`;
  return edit.kind === "clear" && saved ? `${noun[0]?.toUpperCase()}${noun.slice(1)} will be removed` : empty;
}

type ColorChoice = ConnectionColorId | null;
const COLOR_CHOICES: readonly ColorChoice[] = [null, ...CONNECTION_COLORS.map((c) => c.id)];

function ColorPicker({ value, onChange, labelledBy }: { value: ColorChoice; onChange: (c: ColorChoice) => void; labelledBy: string }) {
  const group = useRef<HTMLDivElement>(null);
  const step = (delta: number) => {
    const next = COLOR_CHOICES[(COLOR_CHOICES.indexOf(value) + delta + COLOR_CHOICES.length) % COLOR_CHOICES.length] ?? null;
    onChange(next);
    group.current?.querySelector<HTMLElement>(`[data-testid="conn-color-${next ?? "none"}"]`)?.focus();
  };
  return (
    <div
      ref={group}
      role="radiogroup"
      aria-labelledby={labelledBy}
      className="flex h-8 items-center gap-1.5"
      onKeyDown={(e) => {
        const delta = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
        if (delta === undefined) return;
        e.preventDefault();
        step(delta);
      }}
    >
      {COLOR_CHOICES.map((id) => {
        const color = CONNECTION_COLORS.find((c) => c.id === id);
        const checked = id === value;
        return (
          <button
            key={id ?? "none"}
            type="button"
            role="radio"
            aria-checked={checked}
            aria-label={color?.label ?? "No color"}
            tabIndex={checked ? 0 : -1}
            data-testid={`conn-color-${id ?? "none"}`}
            onClick={() => onChange(id)}
            className={cx("grid size-6 place-items-center rounded-md", checked && "ring-2 ring-run ring-offset-2 ring-offset-raised")}
          >
            {color ? (
              <span className="size-4 rounded-[4px]" style={{ backgroundColor: color.hex }} />
            ) : (
              <span className="size-4 rounded-[4px] border border-dashed border-dim" />
            )}
          </button>
        );
      })}
    </div>
  );
}

function ConnectionForm({
  existing,
  onSaved,
  onClose,
  onDirtyChange,
}: {
  existing: ConnectionConfig | undefined;
  onSaved: () => void;
  onClose: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const upsertConfig = useWorkspace((s) => s.upsertConfig);
  const ids = useId();
  const [fields, setFields] = useState(() => initialFields(existing));
  const [password, setPassword] = useState<PasswordEdit>(() => initialPassword(existing));
  const [sshSecret, setSshSecret] = useState<PasswordEdit>(() => initialSshSecret(existing));
  const [url, setUrl] = useState("");
  const [urlError, setUrlError] = useState<string | null>(null);
  const [urlBlurred, setUrlBlurred] = useState(false);
  const urlRequest = useRef(0);
  const probeRequest = useRef(0);
  const [probe, setProbe] = useState<Probe>({ kind: "idle" });
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(() => !!existing?.ssh || !!existing?.caCertPath);

  const dirty = isDirty(existing, { fields, password, sshSecret, url });
  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);

  const resetProbe = () => {
    ++probeRequest.current;
    setProbe({ kind: "idle" });
  };
  const update = (patch: Partial<Fields>) => {
    resetProbe();
    setFields((f) => ({ ...f, ...patch }));
  };
  const set =
    (key: "name" | "host" | "port" | "database" | "user" | "sshHost" | "sshPort" | "sshUser" | "sshKeyPath" | "sshAgentSocket") =>
    (e: React.ChangeEvent<HTMLInputElement>) =>
      update({ [key]: e.target.value });
  const editSshSecret = (edit: PasswordEdit) => {
    resetProbe();
    setSshSecret(edit);
  };

  const parseUrl = async (text: string) => {
    const request = ++urlRequest.current;
    if (text.trim() === "") {
      setUrlError(null);
      return;
    }
    const res = await commands.connectionParseUrl(text);
    if (request !== urlRequest.current) return;
    if (res.status === "error") {
      setUrlError(res.error);
      return;
    }
    const d = res.data;
    setUrlError(null);
    if (d.caCertPath) setAdvancedOpen(true);
    update({
      databaseKind: d.databaseKind,
      caCertPath: d.caCertPath ?? "",
      host: d.host,
      port: String(d.port),
      database: d.database,
      user: d.user,
      sqlServerAuth: d.sqlServerAuth ?? "SqlLogin",
      sslMode: d.sslMode,
    });
    setPassword(typeof d.password === "object" ? { kind: "set", value: d.password.Set } : initialPassword(existing));
  };
  // A half-typed URL fails to parse, so its error waits for a blur or until it reads like a URL or a key=value string.
  const shownUrlError = urlError !== null && (urlBlurred || url.includes("://") || url.includes("=")) ? urlError : null;

  const port = parsePort(fields.port);
  const valid = fields.host.trim() !== "" && port !== null && sshValid(fields) && toSqlServerAuth(fields) !== "Unknown" && !entraNeedsVerifiedTls(fields);
  const fallbackName = `${fields.host.trim()} / ${fields.database.trim()}`;

  const draft = (): ConnectionDraft => ({
    id: existing?.id ?? null,
    databaseKind: fields.databaseKind,
    name: fields.name.trim() || fallbackName,
    host: fields.host.trim(),
    port: port ?? 0,
    database: fields.database.trim(),
    user: toUser(fields),
    sqlServerAuth: toSqlServerAuth(fields),
    sslMode: fields.sslMode,
    caCertPath: fields.caCertPath.trim() || null,
    password: toPassword(fields, password),
    color: fields.color,
    ssh: toSshDraft(fields, sshSecret),
    environment: fields.environment,
  });

  const test = async () => {
    const request = ++probeRequest.current;
    setProbe({ kind: "testing" });
    const res = await commands.connectionTest(draft());
    if (request !== probeRequest.current) return;
    if (res.status === "ok") {
      setProbe({ kind: "ok", version: res.data.version });
      return;
    }
    const key = unknownHostKey(res.error);
    setProbe(key ? { kind: "hostKey", key } : { kind: "error", message: connectErrorText(res.error) });
  };

  const trustAndTest = async (key: UnknownHostKey) => {
    const request = ++probeRequest.current;
    setProbe({ kind: "testing" });
    const res = await commands.sshTrustHostKey(key.trust);
    if (request !== probeRequest.current) return;
    if (res.status === "error") setProbe({ kind: "error", message: `SSH: ${res.error}` });
    else await test();
  };

  const submit = async () => {
    setSaving(true);
    setSaveError(null);
    const res = await commands.connectionSave(draft());
    setSaving(false);
    if (res.status === "error") {
      setSaveError(res.error.message);
      return;
    }
    upsertConfig(res.data);
    onSaved();
    if (!existing) void connect(res.data.id);
  };

  const sshSecretSaved = initialSshSecret(existing, fields.sshAuth).kind === "keep";
  const sshNoun = fields.sshAuth === "Key" ? "passphrase" : "password";
  const sshSecretInput = (empty: string) => (
    <SecretInput
      testId="conn-ssh-secret"
      label={fields.sshAuth === "Key" ? "Passphrase" : "SSH password"}
      value={sshSecret.kind === "set" ? sshSecret.value : ""}
      placeholder={secretPlaceholder(sshSecret, sshSecretSaved, sshNoun, empty)}
      onChange={(value) => editSshSecret(value ? { kind: "set", value } : initialSshSecret(existing, fields.sshAuth))}
    />
  );
  const sshSecretClear = sshSecretSaved && (
    <ClearSecretButton
      testId="conn-ssh-secret-clear"
      noun={sshNoun}
      cleared={sshSecret.kind === "clear"}
      onToggle={() => editSshSecret(sshSecret.kind === "clear" ? { kind: "keep" } : { kind: "clear" })}
    />
  );
  const entra = signsInWithEntra(fields);
  const sslModes = fields.databaseKind === "SqlServer" ? SSL_MODES.filter((m) => m.value === "VerifyFull" || m.value === "Require") : SSL_MODES;
  const sslHint = entraNeedsVerifiedTls(fields) ? (
    <span data-testid="conn-ssl-entra" className="text-danger">
      Choose Verify full for Microsoft Entra ID. Its token works on every Azure SQL database you can reach, so it goes only to a verified server. Azure SQL
      certificates verify.
    </span>
  ) : (
    sslModes.find((m) => m.value === fields.sslMode)?.hint
  );

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (valid) void submit();
      }}
      className="flex min-w-0 flex-1 flex-col"
    >
      <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border pr-3 pl-5">
        <DatabaseBadge databaseKind={fields.databaseKind} />
        <Dialog.Title className="min-w-0 flex-1 truncate text-ui-14 font-semibold text-fg">{existing ? existing.name : "New connection"}</Dialog.Title>
        <Dialog.Description className="sr-only">Saved connections, and the settings of the selected one.</Dialog.Description>
        <Dialog.Close asChild>
          <IconButton aria-label="Close">
            <XIcon size={15} />
          </IconButton>
        </Dialog.Close>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-1">
        <FormRow label="Database kind" htmlFor={`${ids}-kind`} hint={existing ? "Create a new connection to use another database kind." : undefined}>
          <select
            id={`${ids}-kind`}
            aria-describedby={existing ? hintOf(`${ids}-kind`) : undefined}
            data-testid="conn-kind"
            className={inputClass}
            value={fields.databaseKind}
            disabled={existing !== undefined}
            onChange={(e) => {
              const databaseKind = e.target.value;
              if (databaseKind !== "Postgres" && databaseKind !== "SqlServer") return;
              ++urlRequest.current;
              setUrl("");
              setUrlError(null);
              const { port, database, user, sqlServerAuth, sslMode, caCertPath } = initialFields(undefined, databaseKind);
              update({ databaseKind, port, database, user, sqlServerAuth, sslMode, caCertPath });
              setPassword({ kind: "clear" });
            }}
          >
            {Object.entries(DATABASE_LABEL).map(([kind, label]) => (
              <option key={kind} value={kind}>
                {label}
              </option>
            ))}
          </select>
        </FormRow>
        <FormRow label="Name" htmlFor={`${ids}-name`} hint="Leave empty to use the host and database.">
          <input
            id={`${ids}-name`}
            aria-describedby={hintOf(`${ids}-name`)}
            data-testid="conn-name"
            autoFocus={existing !== undefined}
            className={inputClass}
            value={fields.name}
            onChange={set("name")}
            placeholder={fallbackName}
          />
        </FormRow>
        <FormRow
          label="Environment"
          htmlFor={`${ids}-environment`}
          hint="Staging uses a yellow Run button; Production uses red. On Production, a run that writes asks first."
        >
          <div className="relative">
            <select
              id={`${ids}-environment`}
              aria-describedby={hintOf(`${ids}-environment`)}
              data-testid="conn-environment"
              className={cx(inputClass, "appearance-none pr-8")}
              value={fields.environment ?? ""}
              onChange={(e) => {
                const choice = ENVIRONMENTS.find((env) => (env.value ?? "") === e.target.value);
                // The environment never changes how the connection dials, so a Test result still holds.
                if (choice) setFields((f) => ({ ...f, environment: choice.value }));
              }}
            >
              {ENVIRONMENTS.map((env) => (
                <option key={env.label} value={env.value ?? ""}>
                  {env.label}
                </option>
              ))}
            </select>
            <ChevronsUpDownIcon size={14} className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-dim" />
          </div>
        </FormRow>
        {!existing && (
          <FormRow
            label="URL"
            htmlFor={`${ids}-url`}
            hint={shownUrlError ? <span className="text-danger">{shownUrlError}</span> : "Optional. Paste one to fill in the fields below."}
            live
          >
            <input
              id={`${ids}-url`}
              aria-describedby={hintOf(`${ids}-url`)}
              aria-invalid={shownUrlError !== null || undefined}
              data-testid="conn-url"
              autoFocus
              spellCheck={false}
              className={cx(inputClass, "font-mono")}
              value={url}
              placeholder={fields.databaseKind === "SqlServer" ? "sqlserver://user@host:1433/master" : "postgres://user@host:5432/database"}
              onChange={(e) => {
                setUrl(e.target.value);
                void parseUrl(e.target.value);
              }}
              onBlur={() => setUrlBlurred(true)}
            />
          </FormRow>
        )}
        <FormRow label="Server" htmlFor={`${ids}-host`} hint={fields.sshEnabled ? "Resolved and dialed by the SSH server." : undefined}>
          <div className="flex gap-2">
            <div className="min-w-0 flex-1">
              <input
                id={`${ids}-host`}
                aria-describedby={fields.sshEnabled ? hintOf(`${ids}-host`) : undefined}
                data-testid="conn-host"
                className={inputClass}
                value={fields.host}
                onChange={set("host")}
              />
            </div>
            <div className="w-24 shrink-0">
              <input data-testid="conn-port" aria-label="Port" className={inputClass} value={fields.port} onChange={set("port")} inputMode="numeric" />
            </div>
          </div>
        </FormRow>
        <FormRow label="Database" htmlFor={`${ids}-database`} hint="Opens first in the tree. You can show the server's other databases later.">
          <input
            id={`${ids}-database`}
            aria-describedby={hintOf(`${ids}-database`)}
            data-testid="conn-database"
            className={inputClass}
            value={fields.database}
            onChange={set("database")}
          />
        </FormRow>
        {fields.databaseKind === "SqlServer" && (
          <FormRow
            label="Authentication"
            htmlFor={`${ids}-auth`}
            hint={
              <>
                {SQL_SERVER_AUTHS.find((a) => a.value === fields.sqlServerAuth)?.hint}
                {entra && existing?.hasPassword && <span className="block">Saving removes the saved password.</span>}
              </>
            }
          >
            <div className="relative">
              <select
                id={`${ids}-auth`}
                aria-describedby={hintOf(`${ids}-auth`)}
                data-testid="conn-auth"
                className={cx(inputClass, "appearance-none pr-8")}
                value={fields.sqlServerAuth}
                onChange={(e) => {
                  const choice = SQL_SERVER_AUTHS.find((a) => a.value === e.target.value);
                  if (choice) update({ sqlServerAuth: choice.value });
                }}
              >
                {SQL_SERVER_AUTHS.filter((a) => a.value !== "Unknown" || fields.sqlServerAuth === "Unknown").map((a) => (
                  <option key={a.value} value={a.value} disabled={a.value === "Unknown"}>
                    {a.label}
                  </option>
                ))}
              </select>
              <ChevronsUpDownIcon size={14} className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-dim" />
            </div>
          </FormRow>
        )}
        {takesPassword(fields) && (
          <FormRow
            label={fields.databaseKind === "SqlServer" ? "SQL login" : "Sign in"}
            htmlFor={`${ids}-user`}
            hint={
              existing?.hasPassword && (
                <ClearSecretButton
                  testId="conn-password-clear"
                  noun="password"
                  cleared={password.kind === "clear"}
                  onToggle={() => {
                    resetProbe();
                    setPassword(password.kind === "clear" ? { kind: "keep" } : { kind: "clear" });
                  }}
                />
              )
            }
          >
            <div className="flex gap-2">
              <input id={`${ids}-user`} data-testid="conn-user" className={inputClass} value={fields.user} onChange={set("user")} placeholder="user" />
              <SecretInput
                testId="conn-password"
                label="Password"
                value={password.kind === "set" ? password.value : ""}
                placeholder={secretPlaceholder(password, existing?.hasPassword ?? false, "password", "password")}
                onChange={(value) => {
                  resetProbe();
                  setPassword(value ? { kind: "set", value } : initialPassword(existing));
                }}
              />
            </div>
          </FormRow>
        )}
        <FormRow label="TLS" htmlFor={`${ids}-ssl`} hint={sslHint}>
          <div className="relative">
            <select
              id={`${ids}-ssl`}
              aria-describedby={hintOf(`${ids}-ssl`)}
              aria-invalid={entraNeedsVerifiedTls(fields) || undefined}
              data-testid="conn-ssl"
              className={cx(inputClass, "appearance-none pr-8")}
              value={fields.sslMode}
              onChange={(e) => {
                const mode = sslModes.find((m) => m.value === e.target.value);
                if (mode) update({ sslMode: mode.value });
              }}
            >
              {sslModes.map((m) => (
                <option key={m.value} value={m.value}>
                  {m.label}
                </option>
              ))}
            </select>
            <ChevronsUpDownIcon size={14} className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-dim" />
          </div>
        </FormRow>
        <details open={advancedOpen} onToggle={(e) => setAdvancedOpen(e.currentTarget.open)} className="border-b border-border py-3">
          <summary className="cursor-pointer rounded text-ui-13 font-medium text-fg">
            Advanced network settings
            {(fields.sshEnabled || fields.caCertPath) && (
              <span className="ml-2 text-ui-12 font-normal text-dim">
                {fields.sshEnabled ? (sshValid(fields) ? "SSH configured" : "SSH needs attention") : "CA certificate configured"}
              </span>
            )}
          </summary>
          {fields.databaseKind === "Postgres" && (
            <FormRow label="CA certificate" htmlFor={`${ids}-ca`} hint="Optional path to a PEM certificate file.">
              <input
                id={`${ids}-ca`}
                aria-describedby={hintOf(`${ids}-ca`)}
                data-testid="conn-ca-cert"
                className={inputClass}
                value={fields.caCertPath}
                onChange={(e) => update({ caCertPath: e.target.value })}
              />
            </FormRow>
          )}
          <FormRow
            label="SSH tunnel"
            htmlFor={fields.sshEnabled ? `${ids}-ssh-host` : undefined}
            hint={fields.sshEnabled ? "Checks ~/.ssh/known_hosts. Does not read ~/.ssh/config." : undefined}
          >
            <div className="flex h-8 items-center gap-2">
              <Switch checked={fields.sshEnabled} onCheckedChange={(sshEnabled) => update({ sshEnabled })} label="SSH tunnel" testId="conn-ssh-enabled" />
              {fields.sshEnabled ? (
                <>
                  <div className="ml-1 min-w-0 flex-1">
                    <input
                      id={`${ids}-ssh-host`}
                      aria-describedby={hintOf(`${ids}-ssh-host`)}
                      data-testid="conn-ssh-host"
                      className={inputClass}
                      value={fields.sshHost}
                      onChange={set("sshHost")}
                      placeholder="bastion.example.com"
                    />
                  </div>
                  <div className="w-24 shrink-0">
                    <input
                      data-testid="conn-ssh-port"
                      aria-label="SSH port"
                      className={inputClass}
                      value={fields.sshPort}
                      onChange={set("sshPort")}
                      inputMode="numeric"
                    />
                  </div>
                </>
              ) : (
                <span className="text-ui-13 text-dim">Connect through an SSH server</span>
              )}
            </div>
          </FormRow>
          {fields.sshEnabled && (
            <>
              <FormRow label="SSH sign in" htmlFor={`${ids}-ssh-user`}>
                <div className="flex gap-2">
                  <input
                    id={`${ids}-ssh-user`}
                    data-testid="conn-ssh-user"
                    className={inputClass}
                    value={fields.sshUser}
                    onChange={set("sshUser")}
                    placeholder="user"
                  />
                  <div className="relative w-full">
                    <select
                      data-testid="conn-ssh-auth"
                      aria-label="SSH authentication"
                      className={cx(inputClass, "appearance-none pr-8")}
                      value={fields.sshAuth}
                      onChange={(e) => {
                        const auth = SSH_AUTHS.find((a) => a.value === e.target.value);
                        if (!auth) return;
                        update({ sshAuth: auth.value });
                        setSshSecret(initialSshSecret(existing, auth.value));
                      }}
                    >
                      {SSH_AUTHS.map((a) => (
                        <option key={a.value} value={a.value}>
                          {a.label}
                        </option>
                      ))}
                    </select>
                    <ChevronsUpDownIcon size={14} className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-dim" />
                  </div>
                </div>
              </FormRow>
              {fields.sshAuth === "Password" && (
                <FormRow label="SSH password" hint={sshSecretClear || undefined}>
                  {sshSecretInput("password")}
                </FormRow>
              )}
              {fields.sshAuth === "Key" && (
                <FormRow
                  label="Key file"
                  htmlFor={`${ids}-ssh-key`}
                  hint={
                    <>
                      <span className="block">Leave the passphrase empty for an unencrypted key.</span>
                      {sshSecretClear}
                    </>
                  }
                >
                  <div className="flex gap-2">
                    <div className="min-w-0 flex-1">
                      <input
                        id={`${ids}-ssh-key`}
                        aria-describedby={hintOf(`${ids}-ssh-key`)}
                        data-testid="conn-ssh-key"
                        className={inputClass}
                        value={fields.sshKeyPath}
                        onChange={set("sshKeyPath")}
                        placeholder="~/.ssh/id_ed25519"
                      />
                    </div>
                    <div className="w-40 shrink-0">{sshSecretInput("passphrase")}</div>
                  </div>
                </FormRow>
              )}
              {fields.sshAuth === "Agent" && (
                <FormRow label="Agent socket" htmlFor={`${ids}-ssh-agent`} hint="Optional. Leave empty to use the agent at SSH_AUTH_SOCK.">
                  <input
                    id={`${ids}-ssh-agent`}
                    aria-describedby={hintOf(`${ids}-ssh-agent`)}
                    data-testid="conn-ssh-agent-socket"
                    className={inputClass}
                    value={fields.sshAgentSocket}
                    onChange={set("sshAgentSocket")}
                    placeholder="SSH_AUTH_SOCK"
                  />
                </FormRow>
              )}
            </>
          )}
        </details>
        <FormRow label="Color" labelId={`${ids}-color`} hint="Marks this connection in the tree and the top bar.">
          <ColorPicker value={fields.color} onChange={(color) => update({ color })} labelledBy={`${ids}-color`} />
        </FormRow>
      </div>

      {probe.kind === "hostKey" && <HostKeyCard hostKey={probe.key} onTrust={() => void trustAndTest(probe.key)} />}
      {probe.kind === "error" && <ErrorRow testId="conn-test-error" message={probe.message} />}
      {saveError && <ErrorRow testId="conn-save-error" message={saveError} />}
      <div className="flex shrink-0 items-center gap-3 border-t border-border px-5 py-3">
        <Button data-testid="conn-test" disabled={!valid || probe.kind === "testing"} onClick={() => void test()}>
          Test
        </Button>
        <div data-testid="conn-test-result" data-state={probe.kind} role="status" className="min-w-0 flex-1 text-ui-12.5 leading-4">
          {probe.kind === "testing" && (
            <span className="flex items-center gap-2 text-dim">
              <Spinner /> Connecting
            </span>
          )}
          {probe.kind === "ok" && (
            <span className="flex items-center gap-1.5 text-fg">
              <CheckIcon size={14} className="shrink-0 text-green" />
              <span className="truncate">
                Connected to {DATABASE_LABEL[fields.databaseKind]} {probe.version}
              </span>
            </span>
          )}
        </div>
        <Button variant="ghost" onClick={onClose}>
          Close
        </Button>
        <Button data-testid="conn-save" type="submit" variant="primary" disabled={!valid || saving}>
          {existing ? "Save" : "Save and connect"}
        </Button>
      </div>
    </form>
  );
}

function HostKeyCard({ hostKey, onTrust }: { hostKey: UnknownHostKey; onTrust: () => void }) {
  const fingerprint = `${hostKey.algorithm} ${hostKey.fingerprint}`;
  return (
    <div data-testid="ssh-host-key" className="flex shrink-0 items-center gap-3 border-t border-border px-5 py-3">
      <KeyIcon size={16} className="shrink-0 text-amber" />
      <div className="min-w-0 flex-1 text-ui-12.5 leading-4">
        <div className="truncate text-fg">
          {hostKey.host}:{hostKey.port} offered an unknown host key
        </div>
        <div className="selectable mt-1 font-mono text-ui-11 break-all text-dim">{fingerprint}</div>
      </div>
      <Button data-testid="ssh-trust" onClick={onTrust}>
        Trust and test again
      </Button>
    </div>
  );
}

function ErrorRow({ testId, message }: { testId: string; message: string }) {
  return (
    <div
      data-testid={testId}
      role="alert"
      className="flex max-h-40 shrink-0 items-start gap-2 overflow-y-auto border-t border-border px-5 py-3 text-ui-12.5 leading-4 text-danger"
    >
      <CircleAlertIcon size={14} className="shrink-0" />
      <span className="selectable min-w-0 break-words">{message}</span>
    </div>
  );
}

export function ConnectionDialog() {
  const overlay = useWorkspace((s) => s.overlay);
  const configs = useWorkspace((s) => s.configs);
  const close = useWorkspace((s) => s.close);
  const open = useWorkspace((s) => s.open);
  const editing = overlay.kind === "connection" ? overlay.editing : null;
  const existing = editing ? configs[editing] : undefined;
  const selected = existing ? editing : null;
  const [dirty, setDirty] = useState(false);
  const [pending, setPending] = useState<(() => void) | null>(null);
  const keepEditing = useRef<HTMLButtonElement>(null);

  const leave = (action: () => void) => {
    if (dirty) setPending(() => action);
    else action();
  };
  const select = (id: ConnectionId | null) => {
    if (id !== selected) leave(() => open({ kind: "connection", editing: id }));
  };
  const discard = () => {
    const action = pending;
    setPending(null);
    action?.();
  };

  return (
    <Dialog.Root open={overlay.kind === "connection"} onOpenChange={(o) => !o && leave(close)}>
      <Dialog.Portal>
        <DialogSurface data-testid="connection-dialog" className="flex h-[560px] w-[760px] overflow-hidden">
          <ConnectionList selected={selected} onSelect={select} />
          <ConnectionForm key={editing ?? "new"} existing={existing} onSaved={close} onClose={() => leave(close)} onDirtyChange={setDirty} />
          <Modal
            open={pending !== null}
            onOpenChange={(o) => !o && setPending(null)}
            title="Discard unsaved changes?"
            description={existing ? `Your edits to ${existing.name} are lost.` : "This connection is not saved yet."}
            width={400}
            testId="discard-dialog"
            role="alertdialog"
            initialFocus={keepEditing}
          >
            <div className="flex justify-end gap-2">
              <Button ref={keepEditing} data-testid="discard-cancel" variant="ghost" onClick={() => setPending(null)}>
                Keep editing
              </Button>
              <Button data-testid="discard-confirm" variant="danger" onClick={discard}>
                Discard
              </Button>
            </div>
          </Modal>
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
