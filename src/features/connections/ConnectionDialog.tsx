import * as Dialog from "@radix-ui/react-dialog";
import {
  CheckIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ChevronsUpDownIcon,
  CircleAlertIcon,
  EyeIcon,
  EyeOffIcon,
  InfoIcon,
  KeyIcon,
  LockIcon,
  XIcon,
} from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import {
  commands,
  type ConnectionConfig,
  type ConnectionDraft,
  type DatabaseKind,
  type Environment,
  type SqlServerAuth,
  type SslMode,
} from "../../ipc/bindings";
import { connectErrorText } from "../../ipc/errors";
import { DATABASE_LABEL } from "../../state/databaseKind";
import { useWorkspace } from "../../state/workspace";
import { Button, cx, DatabaseBadge, DialogSurface, IconButton, inputClass, Modal, Spinner, Switch } from "../../ui/primitives";
import { connect, unknownHostKey, type UnknownHostKey } from "./connectionActions";
import { CONNECTION_COLORS, type ConnectionColorId } from "./connectionColors";
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
  urlPassword,
  withUrl,
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

const sslMode = (value: SslMode) => SSL_MODES.find((m) => m.value === value);

const ENVIRONMENTS: SegmentOption<Environment | null>[] = [
  { value: null, label: "Not set" },
  { value: "Local", label: "Develop" },
  { value: "Staging", label: "Staging", selectedClass: "text-amber" },
  { value: "Production", label: "Production", selectedClass: "text-danger" },
];

const ENVIRONMENT_HINT: Record<Environment | "none", string> = {
  none: "Staging turns Run yellow. Production turns it red and asks before a run that writes.",
  Local: "Run stays blue.",
  Staging: "Run turns yellow.",
  Production: "Run turns red, and a run that writes asks first.",
};

const SQL_SERVER_AUTHS: SegmentOption<SqlServerAuth>[] = [
  { value: "SqlLogin", label: "SQL login" },
  { value: "EntraAzureCli", label: "Microsoft Entra ID" },
];

const SSH_AUTHS: SegmentOption<SshAuthKind>[] = [
  { value: "Password", label: "Password" },
  { value: "Key", label: "Key file" },
  { value: "Agent", label: "SSH agent" },
];

type UrlSeed = { draft: ConnectionDraft; url: string };

/** The chooser's URL, when it picked the kind, seeds the form. */
type Step = { kind: "choose" } | { kind: "form"; databaseKind: DatabaseKind; seed: UrlSeed | null };

type Probe =
  | { kind: "idle" }
  | { kind: "testing" }
  | { kind: "ok"; version: string }
  | { kind: "error"; message: string }
  | { kind: "hostKey"; key: UnknownHostKey };

/** What the sign-in and TLS sections of one kind's form read and write. */
type FormApi = {
  ids: string;
  existing: ConnectionConfig | undefined;
  fields: Fields;
  update: (patch: Partial<Fields>) => void;
  password: PasswordEdit;
  editPassword: (edit: PasswordEdit) => void;
};

const sshTarget = (f: Fields) => (f.sshEnabled ? f.sshHost.trim() || null : undefined);

const KINDS: Record<
  DatabaseKind,
  {
    title: string;
    subtitle: string;
    description: string;
    facts: readonly string[];
    urlPlaceholder: string;
    hostPlaceholder: string;
    networkTitle: string;
    networkSummary: (f: Fields) => string;
    SignIn: (api: FormApi) => ReactNode;
    Tls: ((api: FormApi) => ReactNode) | null;
  }
> = {
  Postgres: {
    title: "New PostgreSQL connection",
    subtitle: "Paste a postgres:// URL, or enter the server.",
    description: "Any PostgreSQL server, on your machine or hosted.",
    facts: ["Port 5432", "User and password", "TLS, CA certificate, SSH tunnel"],
    urlPlaceholder: "postgres://user@host:5432/database",
    hostPlaceholder: "db.example.com",
    networkTitle: "TLS, CA certificate, and SSH tunnel",
    networkSummary: (f) => {
      const ssh = sshTarget(f);
      const tunnel = ssh === undefined ? "No tunnel" : ssh === null ? "SSH not set up" : `SSH via ${ssh}`;
      return `TLS ${sslMode(f.sslMode)?.label} · ${tunnel}`;
    },
    SignIn: PostgresSignIn,
    Tls: PostgresTls,
  },
  SqlServer: {
    title: "New SQL Server connection",
    subtitle: "Paste a sqlserver:// URL, or enter the server. Works with Azure SQL Database.",
    description: "SQL Server and Azure SQL Database.",
    facts: ["Port 1433", "SQL login or Microsoft Entra ID", "TLS Require or Verify full, SSH tunnel"],
    urlPlaceholder: "sqlserver://user@host:1433/master",
    hostPlaceholder: "server.database.windows.net",
    networkTitle: "SSH tunnel",
    networkSummary: (f) => {
      const ssh = sshTarget(f);
      return ssh === undefined ? "Off" : ssh === null ? "Not set up" : `via ${ssh}`;
    },
    SignIn: SqlServerSignIn,
    Tls: null,
  },
};

const KIND_ORDER: readonly DatabaseKind[] = ["Postgres", "SqlServer"];

/** The id of the hint a `Field` draws for its control. The control points `aria-describedby` at it. */
const hintOf = (id: string) => `${id}-hint`;

/** `live` reads the hint aloud when it changes, for a hint that turns into the field's error. */
function Field({
  label,
  htmlFor,
  labelId,
  aside,
  hint,
  live,
  className,
  children,
}: {
  label: string;
  htmlFor?: string;
  labelId?: string;
  aside?: ReactNode;
  hint?: ReactNode;
  live?: boolean;
  className?: string;
  children: ReactNode;
}) {
  const id = htmlFor ?? labelId;
  return (
    <div className={cx("min-w-0", className)}>
      <div className="mb-1.5 flex items-baseline justify-between gap-2">
        <label id={labelId} htmlFor={htmlFor} className="text-ui-12 font-medium text-muted">
          {label}
        </label>
        {aside && <span className="text-ui-11.5 text-dim">{aside}</span>}
      </div>
      {children}
      {hint && (
        <div id={id && hintOf(id)} aria-live={live ? "polite" : undefined} className="mt-1.5 text-ui-12 text-dim">
          {hint}
        </div>
      )}
    </div>
  );
}

type SegmentOption<T> = { value: T; label: string; disabled?: boolean; selectedClass?: string };

/** One choice of a few, as a thumb set into a track. A value no option has, such as a sign-in method from a newer version, selects none. */
function Segmented<T extends string | null>({
  options,
  value,
  onChange,
  labelledBy,
  describedBy,
  testId,
}: {
  options: readonly SegmentOption<T>[];
  value: T;
  onChange: (value: T) => void;
  labelledBy: string;
  describedBy?: string;
  testId: string;
}) {
  const group = useRef<HTMLDivElement>(null);
  const enabled = options.filter((o) => !o.disabled);
  const current = enabled.findIndex((o) => o.value === value);
  const tabStop = enabled[Math.max(current, 0)]?.value;
  const optionId = (v: T) => `${testId}-${v ?? "none"}`;
  const step = (delta: number) => {
    const next = enabled[(current + delta + enabled.length) % enabled.length];
    if (!next) return;
    onChange(next.value);
    group.current?.querySelector<HTMLElement>(`[data-testid="${optionId(next.value)}"]`)?.focus();
  };
  return (
    <div
      ref={group}
      role="radiogroup"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
      data-testid={testId}
      className="flex h-8 gap-0.5 rounded-lg bg-segment-track p-[3px] shadow-[inset_0_1px_2px_rgb(0_0_0/0.06)]"
      onKeyDown={(e) => {
        const delta = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
        if (delta === undefined) return;
        e.preventDefault();
        step(delta);
      }}
    >
      {options.map((o) => {
        const checked = o.value === value;
        return (
          <button
            key={optionId(o.value)}
            type="button"
            role="radio"
            aria-checked={checked}
            disabled={o.disabled}
            tabIndex={o.value === tabStop ? 0 : -1}
            data-testid={optionId(o.value)}
            onClick={() => onChange(o.value)}
            className={cx(
              "min-w-0 flex-1 truncate rounded-md border-[0.5px] px-2 text-ui-12.5 transition-colors disabled:pointer-events-none disabled:opacity-40",
              checked
                ? cx("border-segment-thumb-border bg-segment-thumb font-medium shadow-[0_1px_2px_rgb(0_0_0/0.08)]", o.selectedClass ?? "text-fg")
                : "border-transparent text-muted hover:text-fg",
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

function SecretInput({
  id,
  testId,
  label,
  value,
  placeholder,
  onChange,
}: {
  id?: string;
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
        id={id}
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
    <button type="button" data-testid={testId} onClick={onToggle} className="shrink-0 hover:text-fg">
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

/**
 * The URL field's text and its parse. `onParsed` takes a parse that succeeded and returns an error to show instead, or
 * null. A newer edit drops the reply to an older one.
 */
function useUrlField(initial: string) {
  const [url, setUrl] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [blurred, setBlurred] = useState(false);
  const request = useRef(0);
  const edit = async (text: string, onParsed: (d: ConnectionDraft, text: string) => string | null) => {
    setUrl(text);
    const r = ++request.current;
    if (text.trim() === "") {
      setError(null);
      return;
    }
    const res = await commands.connectionParseUrl(text);
    if (r !== request.current) return;
    setError(res.status === "error" ? res.error : onParsed(res.data, text));
  };
  // A half-typed URL fails to parse, so its error waits for a blur or until it reads like a URL or a key=value string.
  const shownError = error !== null && (blurred || url.includes("://") || url.includes("=")) ? error : null;
  return { url, shownError, edit, blur: () => setBlurred(true) };
}

function UrlHint({ error, text }: { error: string | null; text: string }) {
  return error ? <span className="text-danger">{error}</span> : text;
}

function Header({ back, databaseKind, title, subtitle }: { back?: () => void; databaseKind?: DatabaseKind; title: string; subtitle: string }) {
  return (
    <div className="flex shrink-0 items-center gap-3 border-b border-border py-3.5 pr-3 pl-5">
      {back && (
        <IconButton data-testid="conn-back" aria-label="Back" onClick={back} className="-ml-2">
          <ChevronLeftIcon size={16} />
        </IconButton>
      )}
      {databaseKind && <DatabaseBadge databaseKind={databaseKind} large />}
      <div className="min-w-0 flex-1">
        <Dialog.Title className="truncate text-ui-14 font-semibold text-fg">{title}</Dialog.Title>
        <Dialog.Description className="mt-0.5 text-ui-12.5 text-dim">{subtitle}</Dialog.Description>
      </div>
      <Dialog.Close asChild>
        <IconButton aria-label="Close" className="self-start">
          <XIcon size={15} />
        </IconButton>
      </Dialog.Close>
    </div>
  );
}

function KindCard({ databaseKind, onPick }: { databaseKind: DatabaseKind; onPick: () => void }) {
  const kind = KINDS[databaseKind];
  return (
    <button
      type="button"
      data-testid={`conn-kind-${databaseKind.toLowerCase()}`}
      onClick={onPick}
      className="group flex flex-col rounded-xl border border-control bg-bg p-4 text-left transition-colors hover:border-run"
    >
      <span aria-hidden className="flex items-center justify-between">
        <DatabaseBadge databaseKind={databaseKind} large />
        <ChevronRightIcon size={16} className="text-dim transition-colors group-hover:text-run" />
      </span>
      <span className="mt-3 text-ui-14 font-semibold text-fg">{DATABASE_LABEL[databaseKind]}</span>
      <span className="mt-1 text-ui-12.5 text-muted">{kind.description}</span>
      <span className="mt-3 flex flex-1 flex-col justify-end">
        <span className="space-y-1 border-t border-border pt-3 text-ui-12 text-dim">
          {kind.facts.map((fact) => (
            <span key={fact} className="block">
              {fact}
            </span>
          ))}
        </span>
      </span>
    </button>
  );
}

function KindChooser({ onPick }: { onPick: (databaseKind: DatabaseKind, seed: UrlSeed | null) => void }) {
  const ids = useId();
  const url = useUrlField("");
  const pasted = useRef(false);
  const openForm = (draft: ConnectionDraft, text: string) => {
    onPick(draft.databaseKind, { draft, url: text });
    return null;
  };
  return (
    <>
      <Header title="Connect a database" subtitle="Choose what you are connecting to." />
      <div className="min-h-0 overflow-y-auto px-5 py-5">
        <div className="grid grid-cols-2 gap-3">
          {KIND_ORDER.map((kind) => (
            <KindCard key={kind} databaseKind={kind} onPick={() => onPick(kind, null)} />
          ))}
        </div>
        <div className="my-5 flex items-center gap-3 text-ui-12 text-dim">
          <span className="h-px flex-1 bg-border" />
          or
          <span className="h-px flex-1 bg-border" />
        </div>
        <Field
          label="Connection URL"
          htmlFor={`${ids}-url`}
          hint={<UrlHint error={url.shownError} text="Paste a URL to skip this step. It picks the kind and opens that form, filled in." />}
          live
        >
          <input
            id={`${ids}-url`}
            aria-describedby={hintOf(`${ids}-url`)}
            aria-invalid={url.shownError !== null || undefined}
            data-testid="conn-url"
            autoFocus
            spellCheck={false}
            className={cx(inputClass, "font-mono")}
            value={url.url}
            placeholder="postgres://… or sqlserver://…"
            onPaste={() => {
              pasted.current = true;
            }}
            onChange={(e) => {
              // Typing parses as it goes but waits for Enter to leave, since a half-typed URL can already parse.
              const open = pasted.current;
              pasted.current = false;
              void url.edit(e.target.value, open ? openForm : () => null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void url.edit(url.url, openForm);
            }}
            onBlur={url.blur}
          />
        </Field>
      </div>
    </>
  );
}

function PasswordFields({ api, userLabel, note }: { api: FormApi; userLabel: string; note: string }) {
  const { ids, existing, fields, update, password, editPassword } = api;
  return (
    <div>
      <div className="flex gap-3">
        <Field label={userLabel} htmlFor={`${ids}-user`} className="flex-1">
          <input
            id={`${ids}-user`}
            data-testid="conn-user"
            className={inputClass}
            value={fields.user}
            onChange={(e) => update({ user: e.target.value })}
            placeholder="user"
          />
        </Field>
        <Field label="Password" htmlFor={`${ids}-password`} className="flex-1">
          <SecretInput
            id={`${ids}-password`}
            testId="conn-password"
            label="Password"
            value={password.kind === "set" ? password.value : ""}
            placeholder={secretPlaceholder(password, existing?.hasPassword ?? false, "password", "password")}
            onChange={(value) => editPassword(value ? { kind: "set", value } : initialPassword(existing))}
          />
        </Field>
      </div>
      <div className="mt-2 flex items-center gap-1.5 text-ui-12 text-dim">
        <LockIcon size={12} className="shrink-0" />
        <span className="min-w-0 flex-1">{note}</span>
        {existing?.hasPassword && (
          <ClearSecretButton
            testId="conn-password-clear"
            noun="password"
            cleared={password.kind === "clear"}
            onToggle={() => editPassword(password.kind === "clear" ? { kind: "keep" } : { kind: "clear" })}
          />
        )}
      </div>
    </div>
  );
}

function PostgresSignIn(api: FormApi) {
  return <PasswordFields api={api} userLabel="User" note="Encrypted with a key kept in the system keychain." />;
}

function SqlServerSignIn(api: FormApi) {
  const { ids, existing, fields, update } = api;
  const entra = signsInWithEntra(fields);
  const notice =
    fields.sqlServerAuth === "Unknown" ? (
      "A newer version of L1 Data saved this sign-in method. Update L1 Data, or choose another method."
    ) : entraNeedsVerifiedTls(fields) ? (
      <span data-testid="conn-ssl-entra" className="text-danger">
        Choose Verify full for Microsoft Entra ID. Its token works on every Azure SQL database you can reach, so it goes only to a verified server. Azure SQL
        certificates verify.
      </span>
    ) : fields.sslMode === "Require" ? (
      sslMode("Require")?.hint
    ) : null;
  return (
    <div>
      <div className="flex gap-3">
        <Field label="Sign in with" labelId={`${ids}-auth`} className="flex-1">
          <Segmented
            testId="conn-auth"
            labelledBy={`${ids}-auth`}
            describedBy={notice ? hintOf(`${ids}-signin`) : undefined}
            options={SQL_SERVER_AUTHS}
            value={fields.sqlServerAuth}
            onChange={(auth) => update(auth === "EntraAzureCli" ? { sqlServerAuth: auth, sslMode: "VerifyFull" } : { sqlServerAuth: auth })}
          />
        </Field>
        <Field label="Encryption" labelId={`${ids}-ssl`} className="w-[196px] shrink-0">
          <Segmented
            testId="conn-encryption"
            labelledBy={`${ids}-ssl`}
            describedBy={notice ? hintOf(`${ids}-signin`) : undefined}
            options={[
              { value: "Require", label: "Require", disabled: entra },
              { value: "VerifyFull", label: "Verify full" },
            ]}
            value={fields.sslMode}
            onChange={(mode) => update({ sslMode: mode })}
          />
        </Field>
      </div>
      {notice && (
        <div id={hintOf(`${ids}-signin`)} className="mt-1.5 text-ui-12 text-dim">
          {notice}
        </div>
      )}
      {takesPassword(fields) && (
        <div className="mt-4">
          <PasswordFields api={api} userLabel="Login" note="Encrypted in the system keychain. Windows authentication is not supported." />
        </div>
      )}
      {entra && (
        <div data-testid="conn-entra" className="mt-4 flex gap-2.5 rounded-lg border border-run/25 bg-run/8 px-3 py-2.5 text-ui-12.5 leading-[18px]">
          <InfoIcon size={14} className="mt-0.5 shrink-0 text-run" />
          <div className="min-w-0">
            <div className="text-fg">
              Signs in as the account of <code className="font-mono">az login</code>
            </div>
            <div className="mt-0.5 text-dim">No password is saved. Entra ID needs Verify full, so its token goes only to a verified server.</div>
            {existing?.hasPassword && <div className="mt-0.5 text-dim">Saving removes the saved password.</div>}
          </div>
        </div>
      )}
    </div>
  );
}

function PostgresTls({ ids, fields, update }: FormApi) {
  return (
    <>
      <Field label="TLS" htmlFor={`${ids}-ssl`} hint={sslMode(fields.sslMode)?.hint}>
        <div className="relative">
          <select
            id={`${ids}-ssl`}
            aria-describedby={hintOf(`${ids}-ssl`)}
            data-testid="conn-ssl"
            className={cx(inputClass, "appearance-none pr-8")}
            value={fields.sslMode}
            onChange={(e) => {
              const mode = SSL_MODES.find((m) => m.value === e.target.value);
              if (mode) update({ sslMode: mode.value });
            }}
          >
            {SSL_MODES.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
          <ChevronsUpDownIcon size={14} className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-dim" />
        </div>
      </Field>
      <Field label="CA certificate" htmlFor={`${ids}-ca`} aside="Optional" hint="Path to a PEM certificate file.">
        <input
          id={`${ids}-ca`}
          aria-describedby={hintOf(`${ids}-ca`)}
          data-testid="conn-ca-cert"
          className={inputClass}
          value={fields.caCertPath}
          onChange={(e) => update({ caCertPath: e.target.value })}
        />
      </Field>
    </>
  );
}

function SshTunnel({ api, sshSecret, editSshSecret }: { api: FormApi; sshSecret: PasswordEdit; editSshSecret: (edit: PasswordEdit) => void }) {
  const { ids, existing, fields, update } = api;
  const set = (key: "sshHost" | "sshPort" | "sshUser" | "sshKeyPath" | "sshAgentSocket") => (e: React.ChangeEvent<HTMLInputElement>) =>
    update({ [key]: e.target.value });
  const secretSaved = initialSshSecret(existing, fields.sshAuth).kind === "keep";
  const noun = fields.sshAuth === "Key" ? "passphrase" : "password";
  const secretInput = (id: string, empty: string) => (
    <SecretInput
      id={id}
      testId="conn-ssh-secret"
      label={fields.sshAuth === "Key" ? "Passphrase" : "SSH password"}
      value={sshSecret.kind === "set" ? sshSecret.value : ""}
      placeholder={secretPlaceholder(sshSecret, secretSaved, noun, empty)}
      onChange={(value) => editSshSecret(value ? { kind: "set", value } : initialSshSecret(existing, fields.sshAuth))}
    />
  );
  const secretClear = secretSaved && fields.sshAuth !== "Agent" && (
    <div className="text-ui-12 text-dim">
      <ClearSecretButton
        testId="conn-ssh-secret-clear"
        noun={noun}
        cleared={sshSecret.kind === "clear"}
        onToggle={() => editSshSecret(sshSecret.kind === "clear" ? { kind: "keep" } : { kind: "clear" })}
      />
    </div>
  );
  return (
    <div className="space-y-3">
      <div className="flex h-8 items-center gap-2.5">
        <Switch checked={fields.sshEnabled} onCheckedChange={(sshEnabled) => update({ sshEnabled })} label="SSH tunnel" testId="conn-ssh-enabled" />
        <span className="text-ui-13 text-fg">Connect through an SSH server</span>
      </div>
      {fields.sshEnabled && (
        <>
          <div className="flex gap-3">
            <Field label="SSH host" htmlFor={`${ids}-ssh-host`} hint="Checks ~/.ssh/known_hosts. Does not read ~/.ssh/config." className="flex-1">
              <input
                id={`${ids}-ssh-host`}
                aria-describedby={hintOf(`${ids}-ssh-host`)}
                data-testid="conn-ssh-host"
                className={inputClass}
                value={fields.sshHost}
                onChange={set("sshHost")}
                placeholder="bastion.example.com"
              />
            </Field>
            <Field label="SSH port" htmlFor={`${ids}-ssh-port`} className="w-[84px] shrink-0">
              <input
                id={`${ids}-ssh-port`}
                data-testid="conn-ssh-port"
                className={inputClass}
                value={fields.sshPort}
                onChange={set("sshPort")}
                inputMode="numeric"
              />
            </Field>
          </div>
          <div className="flex gap-3">
            <Field label="SSH user" htmlFor={`${ids}-ssh-user`} className="flex-1">
              <input
                id={`${ids}-ssh-user`}
                data-testid="conn-ssh-user"
                className={inputClass}
                value={fields.sshUser}
                onChange={set("sshUser")}
                placeholder="user"
              />
            </Field>
            <Field label="Sign in with" labelId={`${ids}-ssh-auth`} className="w-[264px] shrink-0">
              <Segmented
                testId="conn-ssh-auth"
                labelledBy={`${ids}-ssh-auth`}
                options={SSH_AUTHS}
                value={fields.sshAuth}
                onChange={(auth) => {
                  update({ sshAuth: auth });
                  editSshSecret(initialSshSecret(existing, auth));
                }}
              />
            </Field>
          </div>
          {fields.sshAuth === "Password" && (
            <Field label="SSH password" htmlFor={`${ids}-ssh-secret`}>
              {secretInput(`${ids}-ssh-secret`, "password")}
            </Field>
          )}
          {fields.sshAuth === "Key" && (
            <div className="flex gap-3">
              <Field label="Key file" htmlFor={`${ids}-ssh-key`} hint="Leave the passphrase empty for an unencrypted key." className="flex-1">
                <input
                  id={`${ids}-ssh-key`}
                  aria-describedby={hintOf(`${ids}-ssh-key`)}
                  data-testid="conn-ssh-key"
                  className={inputClass}
                  value={fields.sshKeyPath}
                  onChange={set("sshKeyPath")}
                  placeholder="~/.ssh/id_ed25519"
                />
              </Field>
              <Field label="Passphrase" htmlFor={`${ids}-ssh-secret`} className="w-44 shrink-0">
                {secretInput(`${ids}-ssh-secret`, "passphrase")}
              </Field>
            </div>
          )}
          {fields.sshAuth === "Agent" && (
            <Field label="Agent socket" htmlFor={`${ids}-ssh-agent`} aside="Optional" hint="Leave empty to use the agent at SSH_AUTH_SOCK.">
              <input
                id={`${ids}-ssh-agent`}
                aria-describedby={hintOf(`${ids}-ssh-agent`)}
                data-testid="conn-ssh-agent-socket"
                className={inputClass}
                value={fields.sshAgentSocket}
                onChange={set("sshAgentSocket")}
                placeholder="SSH_AUTH_SOCK"
              />
            </Field>
          )}
          {secretClear}
        </>
      )}
    </div>
  );
}

function ConnectionForm({
  databaseKind,
  seed,
  existing,
  onBack,
  onSaved,
  onClose,
  onDirtyChange,
}: {
  databaseKind: DatabaseKind;
  seed: UrlSeed | null;
  existing: ConnectionConfig | undefined;
  onBack: (() => void) | undefined;
  onSaved: () => void;
  onClose: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const kind = KINDS[databaseKind];
  const upsertConfig = useWorkspace((s) => s.upsertConfig);
  const ids = useId();
  const [fields, setFields] = useState(() => {
    const start = initialFields(existing, databaseKind);
    return seed ? withUrl(start, seed.draft) : start;
  });
  const [password, setPassword] = useState<PasswordEdit>(() => (seed ? urlPassword(seed.draft) : initialPassword(existing)));
  const [sshSecret, setSshSecret] = useState<PasswordEdit>(() => initialSshSecret(existing));
  const url = useUrlField(seed?.url ?? "");
  const probeRequest = useRef(0);
  const [probe, setProbe] = useState<Probe>({ kind: "idle" });
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [networkOpen, setNetworkOpen] = useState(() => !!existing?.ssh || !!existing?.caCertPath || !!seed?.draft.caCertPath);
  const outcome = useRef<HTMLDivElement>(null);

  const dirty = isDirty(existing, { fields, password, sshSecret, url: url.url });
  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);

  // The body scrolls, and a test or save result lands at its bottom.
  useEffect(() => {
    if (probe.kind === "error" || probe.kind === "hostKey" || saveError) outcome.current?.scrollIntoView?.({ block: "nearest" });
  }, [probe, saveError]);

  const resetProbe = () => {
    ++probeRequest.current;
    setProbe({ kind: "idle" });
  };
  const update = (patch: Partial<Fields>) => {
    resetProbe();
    setFields((f) => ({ ...f, ...patch }));
  };
  const editPassword = (edit: PasswordEdit) => {
    resetProbe();
    setPassword(edit);
  };
  const editSshSecret = (edit: PasswordEdit) => {
    resetProbe();
    setSshSecret(edit);
  };
  const api: FormApi = { ids, existing, fields, update, password, editPassword };

  const applyUrl = (d: ConnectionDraft) => {
    if (d.databaseKind !== databaseKind) {
      const other = DATABASE_LABEL[d.databaseKind];
      return `That is a ${other} URL. Go back to choose ${other}.`;
    }
    resetProbe();
    if (d.caCertPath) setNetworkOpen(true);
    setFields((f) => withUrl(f, d));
    setPassword(urlPassword(d));
    return null;
  };

  const port = parsePort(fields.port);
  const sshReady = sshValid(fields);
  const valid = fields.host.trim() !== "" && port !== null && sshReady && toSqlServerAuth(fields) !== "Unknown" && !entraNeedsVerifiedTls(fields);
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

  const environmentHint = ENVIRONMENT_HINT[fields.environment ?? "none"];

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (valid) void submit();
      }}
      className="flex min-h-0 flex-col"
    >
      <Header
        back={onBack}
        databaseKind={databaseKind}
        title={existing ? existing.name : kind.title}
        subtitle={existing ? `${DATABASE_LABEL[databaseKind]} · Saved connection` : kind.subtitle}
      />

      <div className="min-h-0 space-y-4 overflow-y-auto px-5 pt-4 pb-1">
        {!existing && (
          <Field
            label="Connection URL"
            htmlFor={`${ids}-url`}
            aside="Optional"
            hint={<UrlHint error={url.shownError} text="Fills in the fields below." />}
            live
          >
            <input
              id={`${ids}-url`}
              aria-describedby={hintOf(`${ids}-url`)}
              aria-invalid={url.shownError !== null || undefined}
              data-testid="conn-url"
              autoFocus
              spellCheck={false}
              className={cx(inputClass, "font-mono")}
              value={url.url}
              placeholder={kind.urlPlaceholder}
              onChange={(e) => void url.edit(e.target.value, applyUrl)}
              onBlur={url.blur}
            />
          </Field>
        )}

        <div className="flex gap-3">
          <Field label="Host" htmlFor={`${ids}-host`} hint={fields.sshEnabled ? "Resolved and dialed by the SSH server." : undefined} className="flex-1">
            <input
              id={`${ids}-host`}
              aria-describedby={fields.sshEnabled ? hintOf(`${ids}-host`) : undefined}
              data-testid="conn-host"
              className={inputClass}
              value={fields.host}
              onChange={(e) => update({ host: e.target.value })}
              placeholder={kind.hostPlaceholder}
            />
          </Field>
          <Field label="Port" htmlFor={`${ids}-port`} className="w-[84px] shrink-0">
            <input
              id={`${ids}-port`}
              data-testid="conn-port"
              className={inputClass}
              value={fields.port}
              onChange={(e) => update({ port: e.target.value })}
              inputMode="numeric"
            />
          </Field>
          <Field label="Database" htmlFor={`${ids}-database`} className="w-[168px] shrink-0">
            <input
              id={`${ids}-database`}
              data-testid="conn-database"
              className={inputClass}
              value={fields.database}
              onChange={(e) => update({ database: e.target.value })}
            />
          </Field>
        </div>

        <kind.SignIn {...api} />

        <section className="space-y-3 border-t border-border pt-4">
          <h3 className="text-ui-12.5 font-semibold text-fg">How it appears in the sidebar</h3>
          <div className="flex gap-4">
            <Field label="Name" htmlFor={`${ids}-name`} className="flex-1">
              <input
                id={`${ids}-name`}
                data-testid="conn-name"
                autoFocus={existing !== undefined}
                className={inputClass}
                value={fields.name}
                onChange={(e) => update({ name: e.target.value })}
                placeholder={fallbackName}
              />
            </Field>
            <Field label="Color" labelId={`${ids}-color`} className="shrink-0">
              <ColorPicker value={fields.color} onChange={(color) => update({ color })} labelledBy={`${ids}-color`} />
            </Field>
          </div>
          <Field label="Environment" labelId={`${ids}-environment`} hint={environmentHint}>
            <Segmented
              testId="conn-environment"
              labelledBy={`${ids}-environment`}
              describedBy={hintOf(`${ids}-environment`)}
              options={ENVIRONMENTS}
              value={fields.environment}
              // The environment never changes how the connection dials, so a Test result still holds.
              onChange={(environment) => setFields((f) => ({ ...f, environment }))}
            />
          </Field>
        </section>

        <details open={networkOpen} onToggle={(e) => setNetworkOpen(e.currentTarget.open)} className="group border-t border-border">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded py-3 text-ui-13 font-medium text-fg [&::-webkit-details-marker]:hidden">
            <ChevronRightIcon size={14} className="shrink-0 text-dim transition-transform group-open:rotate-90" />
            <span className="flex-1">{kind.networkTitle}</span>
            <span data-testid="conn-network-summary" className={cx("truncate text-ui-12 font-normal", sshReady ? "text-dim" : "text-danger")}>
              {kind.networkSummary(fields)}
            </span>
          </summary>
          <div className="space-y-4 pt-1 pb-4">
            {kind.Tls && <kind.Tls {...api} />}
            <SshTunnel api={api} sshSecret={sshSecret} editSshSecret={editSshSecret} />
          </div>
        </details>

        {(probe.kind === "hostKey" || probe.kind === "error" || saveError) && (
          <div ref={outcome} className="space-y-2 pb-3">
            {probe.kind === "hostKey" && <HostKeyCard hostKey={probe.key} onTrust={() => void trustAndTest(probe.key)} />}
            {probe.kind === "error" && <ErrorCallout testId="conn-test-error" message={probe.message} />}
            {saveError && <ErrorCallout testId="conn-save-error" message={saveError} />}
          </div>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-3 border-t border-border px-5 py-3">
        <Button data-testid="conn-test" disabled={!valid || probe.kind === "testing"} onClick={() => void test()}>
          Test connection
        </Button>
        <div data-testid="conn-test-result" data-state={probe.kind} role="status" className="min-w-0 flex-1 text-ui-12.5 leading-4">
          {probe.kind === "testing" && (
            <span className="flex items-center gap-2 text-dim">
              <Spinner /> Connecting…
            </span>
          )}
          {probe.kind === "ok" && (
            <span className="flex items-center gap-1.5 text-fg">
              <CheckIcon size={14} className="shrink-0 text-green" />
              <span className="truncate">
                Connected · {DATABASE_LABEL[databaseKind]} {probe.version}
              </span>
            </span>
          )}
        </div>
        <Button variant="ghost" onClick={onClose}>
          Cancel
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
    <div data-testid="ssh-host-key" className="flex items-center gap-3 rounded-lg border border-amber/30 bg-amber/8 px-3 py-2.5">
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

function ErrorCallout({ testId, message }: { testId: string; message: string }) {
  return (
    <div
      data-testid={testId}
      role="alert"
      className="flex max-h-40 items-start gap-2 overflow-y-auto rounded-lg border border-danger/30 bg-danger/8 px-3 py-2.5 text-ui-12.5 leading-4 text-danger"
    >
      <CircleAlertIcon size={14} className="shrink-0" />
      <span className="selectable min-w-0 break-words">{message}</span>
    </div>
  );
}

/** Mounts with each opening of the dialog, so a new connection always starts at the chooser. */
function ConnectionSteps({
  existing,
  leave,
  onClose,
  onDirtyChange,
}: {
  existing: ConnectionConfig | undefined;
  leave: (action: () => void) => void;
  onClose: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const [chosen, setChosen] = useState<Step>({ kind: "choose" });
  const step: Step = existing ? { kind: "form", databaseKind: existing.databaseKind, seed: null } : chosen;
  if (step.kind === "choose") return <KindChooser onPick={(databaseKind, seed) => setChosen({ kind: "form", databaseKind, seed })} />;
  return (
    <ConnectionForm
      databaseKind={step.databaseKind}
      seed={step.seed}
      existing={existing}
      onBack={existing ? undefined : () => leave(() => setChosen({ kind: "choose" }))}
      onSaved={onClose}
      onClose={() => leave(onClose)}
      onDirtyChange={onDirtyChange}
    />
  );
}

export function ConnectionDialog() {
  const overlay = useWorkspace((s) => s.overlay);
  const configs = useWorkspace((s) => s.configs);
  const close = useWorkspace((s) => s.close);
  const editing = overlay.kind === "connection" ? overlay.editing : null;
  const existing = editing ? configs[editing] : undefined;
  const [dirty, setDirty] = useState(false);
  const [pending, setPending] = useState<(() => void) | null>(null);
  const keepEditing = useRef<HTMLButtonElement>(null);

  const leave = (action: () => void) => {
    if (dirty) setPending(() => action);
    else action();
  };
  const discard = () => {
    const action = pending;
    setPending(null);
    action?.();
  };

  return (
    <Dialog.Root open={overlay.kind === "connection"} onOpenChange={(o) => !o && leave(close)}>
      <Dialog.Portal>
        <DialogSurface data-testid="connection-dialog" className="flex w-[600px] flex-col overflow-hidden">
          <ConnectionSteps key={editing ?? "new"} existing={existing} leave={leave} onClose={close} onDirtyChange={setDirty} />
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
