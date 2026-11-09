import { DatabaseIcon } from "lucide-react";
import type { Environment } from "../../ipc/bindings";
import { cx } from "../../ui/primitives";

/** `ConnectionConfig.color` stores an `id`. The hues are fixed so they read on light and dark themes alike. */
export const CONNECTION_COLORS = [
  { id: "red", label: "Red", hex: "#ef4444" },
  { id: "amber", label: "Amber", hex: "#d97706" },
  { id: "green", label: "Green", hex: "#059669" },
  { id: "blue", label: "Blue", hex: "#3b82f6" },
  { id: "purple", label: "Purple", hex: "#a855f7" },
] as const satisfies readonly { id: string; label: string; hex: string }[];

export type ConnectionColorId = (typeof CONNECTION_COLORS)[number]["id"];

export function connectionColorId(color: string | null): ConnectionColorId | null {
  return CONNECTION_COLORS.find((c) => c.id === color)?.id ?? null;
}

export function connectionColor(color: string | null): string | null {
  return CONNECTION_COLORS.find((c) => c.id === color)?.hex ?? null;
}

/** Nothing when the connection has no known color. */
export function ConnectionColorDot({ color, className }: { color: string | null; className?: string }) {
  const hex = connectionColor(color);
  if (!hex) return null;
  return <span aria-hidden className={cx("inline-block size-2 shrink-0 rounded-[2px]", className)} style={{ backgroundColor: hex }} />;
}

const ENVIRONMENT_ICON: Record<Environment, { label: string; tone: string }> = {
  Local: { label: "Develop", tone: "text-badge" },
  Staging: { label: "Staging", tone: "text-amber" },
  Production: { label: "Production", tone: "text-danger" },
};

const ENVIRONMENT_BADGE: Record<Environment, string> = {
  Local: "bg-control/70 text-muted",
  Staging: "bg-amber/15 text-amber",
  Production: "bg-danger/12 text-danger",
};

/** The environment as a small tinted badge, red for Production so it stands out in a list. Nothing for a connection without one. */
export function EnvironmentBadge({ environment }: { environment: Environment | null }) {
  if (!environment) return null;
  return (
    <span className={cx("ml-2 inline-flex h-4 shrink-0 items-center rounded px-[5px] text-ui-10 leading-none font-medium", ENVIRONMENT_BADGE[environment])}>
      {ENVIRONMENT_ICON[environment].label}
    </span>
  );
}

export function EnvironmentIcon({
  environment,
  size = 14,
  testId = "env-icon",
  className,
}: {
  environment: Environment | null;
  size?: number;
  testId?: string;
  className?: string;
}) {
  const icon = environment ? ENVIRONMENT_ICON[environment] : { label: "Not set", tone: "text-muted" };
  return (
    <span
      data-testid={testId}
      role="img"
      aria-label={icon.label}
      title={icon.label}
      className={cx("inline-grid shrink-0 place-items-center", icon.tone, className)}
    >
      <DatabaseIcon size={size} aria-hidden />
    </span>
  );
}
