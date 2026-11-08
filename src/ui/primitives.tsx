import * as ContextMenu from "@radix-ui/react-context-menu";
import * as Dialog from "@radix-ui/react-dialog";
import * as Menu from "@radix-ui/react-dropdown-menu";
import { CheckIcon, ChevronRightIcon, XIcon } from "lucide-react";
import { useLayoutEffect, useRef, type ComponentProps, type KeyboardEvent, type ReactElement, type ReactNode, type RefObject } from "react";
import type { DatabaseKind } from "../ipc/bindings";
import { DATABASE_LABEL } from "../state/databaseKind";
import type { TextRange } from "./textMatch";

export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

export const MENU_PANEL = "motion-pop z-50 min-w-[180px] rounded-lg border border-control bg-raised p-1 shadow-[var(--shadow-popover)]";
/** The option a listbox or tree acts on. `bg-selected` alone is too close to `bg-raised` to see, so a focus-colour ring marks it. */
export const ACTIVE_OPTION = "bg-selected ring-1 ring-inset ring-focus";
export const MENU_ITEM =
  "flex h-7 cursor-default items-center gap-2 rounded-md px-2 text-ui-13 outline-none data-[highlighted]:bg-selected focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-focus";

export function DatabaseBadge({ databaseKind }: { databaseKind: DatabaseKind }) {
  return (
    <span
      title={DATABASE_LABEL[databaseKind]}
      aria-label={DATABASE_LABEL[databaseKind]}
      className="inline-grid h-4 min-w-[20px] place-items-center rounded-[4px] bg-badge-bg px-[3px] font-mono text-ui-10 font-bold leading-none tracking-tight text-badge"
    >
      {databaseKind === "Postgres" ? "PG" : "MS"}
    </span>
  );
}

const KBD_TONE = {
  surface: "bg-pill",
  menu: "bg-control text-muted",
  accent: "bg-run-fg/15 text-run-fg/85",
};

/** `surface` sits on a panel, `menu` on a raised menu, `accent` inside a primary button. */
export function Kbd({ children, tone = "surface", className }: { children: ReactNode; tone?: keyof typeof KBD_TONE; className?: string }) {
  return (
    <kbd className={cx("inline-flex h-[18px] items-center rounded-[4px] px-1.5 font-sans text-ui-10.5 font-medium leading-none", KBD_TONE[tone], className)}>
      {children}
    </kbd>
  );
}

/** `text` with a dim amber mark over each range, as a filter's matches show. */
export function Highlight({ text, ranges }: { text: string; ranges: readonly TextRange[] | null | undefined }) {
  if (!ranges?.length) return text;
  const parts: ReactNode[] = [];
  let at = 0;
  for (const r of ranges) {
    if (r.start > at) parts.push(text.slice(at, r.start));
    parts.push(
      <mark key={r.start} className="rounded-[2px] bg-amber/20 text-inherit">
        {text.slice(r.start, r.end)}
      </mark>,
    );
    at = r.end;
  }
  if (at < text.length) parts.push(text.slice(at));
  return parts;
}

type Tone = "live" | "pending" | "off" | "error";

/** What a connection's dot means, for the places where the dot alone shows its health. */
export const CONNECTION_TONE_LABEL: Record<Tone, string> = { live: "Connected", pending: "Connecting", off: "Disconnected", error: "Connection failed" };

/** Colour alone is not a status, so a dot that stands for one takes a `label`. Without one it is decoration. */
export function StatusDot({ tone, label }: { tone: Tone; label?: string }) {
  const color = { live: "bg-live", pending: "bg-amber animate-pulse", off: "bg-faint", error: "bg-danger" }[tone];
  return (
    <span
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      title={label}
      className={cx("inline-block size-1.5 shrink-0 rounded-full", color)}
    />
  );
}

export function Spinner({ className = "size-3 border-dim" }: { className?: string }) {
  return <span className={cx("inline-block rounded-full border-[1.5px] border-t-transparent", className)} style={{ animation: "spin 0.8s linear infinite" }} />;
}

type ButtonProps = ComponentProps<"button"> & { variant?: "ghost" | "outline" | "primary" | "danger" };

export function Button({ variant = "outline", className, ...rest }: ButtonProps) {
  const look = {
    ghost: "text-muted hover:bg-hover hover:text-fg",
    outline: "border border-control text-fg hover:bg-hover",
    primary: "bg-run text-run-fg hover:bg-run-hover focus-visible:outline-run-fg",
    danger: "border border-control text-danger hover:bg-hover",
  }[variant];
  return (
    <button
      type="button"
      className={cx(
        "inline-flex h-8 w-fit shrink-0 items-center justify-center gap-1.5 rounded-lg px-3 text-center text-ui-13 leading-[18px] whitespace-nowrap transition-colors disabled:pointer-events-none disabled:opacity-40",
        look,
        className,
      )}
      {...rest}
    />
  );
}

export function IconButton({ className, ...rest }: ComponentProps<"button">) {
  return (
    <button
      type="button"
      className={cx(
        "inline-grid size-7 place-items-center rounded-md text-muted transition-colors hover:bg-hover hover:text-fg disabled:pointer-events-none disabled:opacity-40",
        className,
      )}
      {...rest}
    />
  );
}

export function Switch({
  checked,
  onCheckedChange,
  label,
  testId,
}: {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  label: string;
  testId?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      data-testid={testId}
      onClick={() => onCheckedChange(!checked)}
      className={cx(
        "inline-flex h-[18px] w-[30px] shrink-0 items-center rounded-full px-[2px] transition-colors",
        checked ? "bg-run hover:bg-run-hover" : "bg-control hover:bg-handle-hover",
      )}
    >
      <span className={cx("size-[14px] rounded-full bg-run-fg shadow-[0_1px_2px_rgba(0,0,0,0.3)] transition-transform", checked && "translate-x-[12px]")} />
    </button>
  );
}

export function DialogSurface({ className, ...props }: ComponentProps<typeof Dialog.Content>) {
  return (
    <Dialog.Overlay className="motion-fade fixed inset-0 z-40 bg-backdrop/50 backdrop-blur-[6px]">
      <Dialog.Content
        className={cx(
          "motion-pop fixed top-1/2 left-1/2 z-10 max-h-[calc(100vh-48px)] max-w-[calc(100vw-48px)] -translate-x-1/2 -translate-y-1/2 rounded-xl border border-control bg-raised shadow-[var(--shadow-dialog)] outline-none",
          className,
        )}
        {...props}
      />
    </Dialog.Overlay>
  );
}

export function MenuContent({
  children,
  align = "start",
  className,
  onCloseAutoFocus,
}: {
  children: ReactNode;
  align?: "start" | "end";
  className?: string;
  onCloseAutoFocus?: (event: Event) => void;
}) {
  return (
    <Menu.Portal>
      <Menu.Content align={align} sideOffset={6} className={cx(MENU_PANEL, className)} onCloseAutoFocus={onCloseAutoFocus}>
        {children}
      </Menu.Content>
    </Menu.Portal>
  );
}

export function MenuItem({
  children,
  checked,
  onSelect,
  testId,
  danger,
  disabled,
}: {
  children: ReactNode;
  checked?: boolean;
  onSelect?: () => void;
  testId?: string;
  danger?: boolean;
  disabled?: boolean;
}) {
  return (
    <Menu.Item
      data-testid={testId}
      onSelect={onSelect}
      disabled={disabled}
      className={cx(MENU_ITEM, danger ? "text-danger" : "text-fg", "data-[disabled]:opacity-40")}
    >
      <span className="grid w-3.5 place-items-center">{checked && <CheckIcon size={13} className="text-muted" />}</span>
      {children}
    </Menu.Item>
  );
}

export function ContextMenuArea({ menu, children }: { menu: ReactNode; children: ReactElement }) {
  return (
    <ContextMenu.Root modal={false}>
      <ContextMenu.Trigger asChild>{children}</ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content className={MENU_PANEL}>{menu}</ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
}

export function ContextMenuItem({
  children,
  onSelect,
  testId,
  danger,
  disabled,
  keyShortcuts,
}: {
  children: ReactNode;
  onSelect: () => void;
  testId?: string;
  danger?: boolean;
  disabled?: boolean;
  keyShortcuts?: string;
}) {
  return (
    <ContextMenu.Item
      data-testid={testId}
      aria-keyshortcuts={keyShortcuts}
      onSelect={onSelect}
      disabled={disabled}
      className={cx(MENU_ITEM, "pr-3 data-[disabled]:opacity-40", danger ? "text-danger" : "text-fg")}
    >
      {children}
    </ContextMenu.Item>
  );
}

export function ContextSubmenu({ label, children, testId }: { label: ReactNode; children: ReactNode; testId?: string }) {
  return (
    <ContextMenu.Sub>
      <ContextMenu.SubTrigger data-testid={testId} className={cx(MENU_ITEM, "pr-1 text-fg data-[state=open]:bg-selected")}>
        {label}
        <ChevronRightIcon size={14} className="ml-auto text-muted" />
      </ContextMenu.SubTrigger>
      <ContextMenu.Portal>
        <ContextMenu.SubContent sideOffset={6} alignOffset={-5} className={MENU_PANEL}>
          {children}
        </ContextMenu.SubContent>
      </ContextMenu.Portal>
    </ContextMenu.Sub>
  );
}

export function MenuLabel({ children }: { children: ReactNode }) {
  return <Menu.Label className="px-2 pt-1.5 pb-1 text-ui-11.5 text-dim">{children}</Menu.Label>;
}

export function ContextMenuSeparator() {
  return <ContextMenu.Separator className="my-1 h-px bg-control" />;
}

export function Modal({
  open,
  onOpenChange,
  title,
  description,
  children,
  width = 440,
  testId,
  initialFocus,
  onKeyDown,
  role,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  children: ReactNode;
  width?: number;
  testId?: string;
  /** Radix focuses the first control (the close button) unless this names another. */
  initialFocus?: RefObject<HTMLElement | null>;
  onKeyDown?: (e: KeyboardEvent) => void;
  /** `alertdialog` for a question that interrupts work and must be answered, such as a destructive confirm. */
  role?: "dialog" | "alertdialog";
}) {
  // Radix returns focus to a Dialog.Trigger on close. A Modal has none, so it returns focus to whatever had it at open.
  const opener = useRef<Element | null>(null);
  useLayoutEffect(() => {
    if (open) opener.current = document.activeElement;
  }, [open]);
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <DialogSurface
          data-testid={testId}
          // Radix spreads these props over its own role, so an undefined role would erase "dialog".
          {...(role && { role })}
          style={{ width }}
          onKeyDown={onKeyDown}
          onOpenAutoFocus={
            initialFocus &&
            ((e) => {
              e.preventDefault();
              initialFocus.current?.focus();
            })
          }
          onCloseAutoFocus={(e) => {
            e.preventDefault();
            if (document.activeElement !== document.body) return;
            if (opener.current instanceof HTMLElement && opener.current.isConnected) {
              opener.current.focus();
            }
          }}
          className="overflow-auto p-5"
        >
          <div className="mb-4 flex items-start justify-between gap-4">
            <div>
              <Dialog.Title className="text-ui-14 font-semibold text-fg">{title}</Dialog.Title>
              {description ? (
                <Dialog.Description className="mt-1 text-ui-12.5 [overflow-wrap:anywhere] text-dim">{description}</Dialog.Description>
              ) : (
                <Dialog.Description className="sr-only">{title}</Dialog.Description>
              )}
            </div>
            <Dialog.Close asChild>
              <IconButton aria-label="Close" className="-mt-1 -mr-1">
                <XIcon size={15} />
              </IconButton>
            </Dialog.Close>
          </div>
          {children}
        </DialogSurface>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export const inputClass = "h-8 w-full rounded-lg border border-control bg-bg px-2.5 text-ui-13 text-fg outline-none placeholder:text-dim focus:border-focus";
