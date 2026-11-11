import type { ChatId, RestartImpact, TabId, UpdatesOff, UpdateState, UpdateStatus } from "../../ipc/bindings";
import { updateFailureText } from "../../ipc/errors";
import { timeAgo } from "../assistant/assistantText";

/** The step a click takes. Each names the version the user saw, so the engine refuses a stale click. */
export type UpdateAction = { kind: "check" } | { kind: "download"; version: string } | { kind: "install"; version: string };

export type UpdateGlyph = "check" | "spinner" | "available" | "ring" | "restart" | "error";

export type UpdateButtonView = {
  /** null while a step runs: a click does nothing. */
  action: UpdateAction | null;
  glyph: UpdateGlyph;
  /** Whole percent for the ring. null draws an indeterminate ring. */
  percent: number | null;
  label: string;
  title: string;
  detail: string | null;
  /** Draws the accent dot when a click takes the next step of an update. */
  attention: boolean;
};

export function updateAction(status: UpdateStatus): UpdateAction | null {
  switch (status.kind) {
    case "Idle":
    case "UpToDate":
    case "Failed":
      return { kind: "check" };
    case "Available":
      return { kind: "download", version: status.version };
    case "Downloaded":
      return { kind: "install", version: status.version };
    case "Disabled":
    case "Checking":
    case "Downloading":
    case "Installing":
      return null;
    default: {
      const unreachable: never = status;
      return unreachable;
    }
  }
}

function progressText(percent: number | null): string {
  return percent === null ? "" : ` ${percent}%`;
}

export function updateButtonView(status: UpdateStatus): UpdateButtonView | null {
  const view = (glyph: UpdateGlyph, label: string, title: string, detail: string | null = null): UpdateButtonView => ({
    action: updateAction(status),
    glyph,
    percent: null,
    label,
    title,
    detail,
    attention: false,
  });
  switch (status.kind) {
    case "Disabled":
      return null;
    case "Idle":
      return view("check", "Check for update", "Check for update");
    case "UpToDate":
      return view("check", "Check for update", "Check for update", "Up to date");
    case "Checking":
      return view("spinner", "Checking for update", "Checking for update…");
    case "Available": {
      const title = status.downgrade ? `Stable ${status.version} is older than this build` : `${status.version} is available`;
      return { ...view("available", `Download L1 Data ${status.version}`, title, "Click to download"), attention: true };
    }
    case "Downloading":
      return {
        ...view("ring", `Downloading L1 Data ${status.version}`, `Downloading ${status.version}`, status.percent === null ? null : `${status.percent}%`),
        percent: status.percent,
      };
    case "Downloaded":
      return { ...view("restart", `Restart to install L1 Data ${status.version}`, `${status.version} is ready`, "Click to restart"), attention: true };
    case "Installing":
      return view("spinner", "Restarting", "Restarting to install…");
    case "Failed":
      return view("error", "Update failed", updateFailureText(status.failure), "Click to retry");
    default: {
      const unreachable: never = status;
      return unreachable;
    }
  }
}

/** version is null when only one release has changes to list. */
export type ChangeSection = { version: string | null; items: { title: string; pr: string | null }[] };

/** The offered release's changes, then each skipped release's. */
export function changeSections(status: UpdateStatus): ChangeSection[] {
  if (status.kind !== "Available" && status.kind !== "Downloading" && status.kind !== "Downloaded") return [];
  const releases = status.changes.filter((release) => release.items.length > 0);
  return releases.map((release) => ({
    version: releases.length > 1 ? release.version : null,
    items: release.items.map((item) => ({ title: item.title, pr: item.pr === null ? null : `#${item.pr}` })),
  }));
}

export function disabledText(reason: UpdatesOff): string {
  switch (reason) {
    case "DevBuild":
      return "Development builds don't update themselves.";
    case "NotAppImage":
      return "Only the AppImage updates itself. Update this install with your package manager.";
    case "UnsupportedTarget":
      return "Updates aren't published for this system.";
    case "UnknownVersion":
      return "This build's version can't be read, so it can't update itself.";
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}

export function aboutStatusText(u: UpdateState, nowMs: number): string {
  const status = u.status;
  switch (status.kind) {
    case "Disabled":
      return disabledText(status.reason);
    case "Idle":
      return "Not checked yet.";
    case "Checking":
      return "Checking for updates…";
    case "UpToDate":
      return u.checkedAtMs === null ? "Up to date." : `Up to date. Checked ${timeAgo(nowMs, u.checkedAtMs)}.`;
    case "Available":
      return status.downgrade ? `${status.version}, the latest Stable build, is available. It is older than this nightly.` : `${status.version} is available.`;
    case "Downloading":
      return `Downloading ${status.version}…${progressText(status.percent)}`;
    case "Downloaded":
      return `${status.version} is ready to install.`;
    case "Installing":
      return `Restarting into ${status.version}…`;
    case "Failed":
      return updateFailureText(status.failure);
    default: {
      const unreachable: never = status;
      return unreachable;
    }
  }
}

export function actionLabel(status: UpdateStatus): string {
  switch (status.kind) {
    case "Available":
      return `Download ${status.version}`;
    case "Downloaded":
      return "Restart to install";
    case "Checking":
      return "Checking…";
    case "Downloading":
      return "Downloading…";
    case "Installing":
      return "Restarting…";
    case "Disabled":
    case "Idle":
    case "UpToDate":
    case "Failed":
      return "Check for updates";
    default: {
      const unreachable: never = status;
      return unreachable;
    }
  }
}

export function restartTitle(version: string, u: UpdateState | null): string {
  const older = u?.status.kind === "Downloaded" && u.status.downgrade;
  return older ? `Restart to install L1 Data ${version}, an older build?` : `Restart to install L1 Data ${version}?`;
}

function listOf(items: readonly string[]): string {
  if (items.length <= 2) return items.join(" and ");
  return `${items.slice(0, -1).join(", ")}, and ${items.at(-1)}`;
}

function quoted(titles: readonly string[]): string {
  return listOf(titles.map((t) => `“${t}”`));
}

export function restartBody(impact: RestartImpact, unsubmitted: readonly TabId[], tabTitle: (id: TabId) => string, chatTitle: (id: ChatId) => string): string {
  const changed = impact.transactions.filter((t) => t.hasWrites).map((t) => tabTitle(t.tabId));
  const open = impact.transactions.filter((t) => !t.hasWrites).map((t) => tabTitle(t.tabId));
  const ends = [
    impact.running.length > 0 && `cancels the ${impact.running.length === 1 ? "query" : "queries"} running in ${quoted(impact.running.map(tabTitle))}`,
    changed.length > 0 && `rolls back the uncommitted changes in ${quoted(changed)}`,
    unsubmitted.length > 0 && `discards the changes in ${quoted(unsubmitted.map(tabTitle))} that haven't been submitted`,
    open.length > 0 && `ends the open ${open.length === 1 ? "transaction" : "transactions"} in ${quoted(open)}`,
    impact.chats.length > 0 && `stops the assistant in ${quoted(impact.chats.map(chatTitle))}`,
  ].filter((clause) => clause !== false);
  const reopen = "L1 Data will close and reopen.";
  return ends.length === 0 ? reopen : `Restarting ${listOf(ends)}. ${reopen}`;
}
