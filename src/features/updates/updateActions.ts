import { commands, type ChatId, type RestartImpact, type TabId, type UpdateError, type UpdateTrack } from "../../ipc/bindings";
import { updateErrorText } from "../../ipc/errors";
import { useLive } from "../../state/live";
import { pendingCount, useTableEdits } from "../../state/tableEdits";
import { allCells, confirm, useWorkspace } from "../../state/workspace";
import { flushWorkspaceSave } from "../editor/persistence";
import { restartBody, restartTitle, type UpdateAction } from "./updateText";

const ws = () => useWorkspace.getState();

function notifyUnlessStale(e: UpdateError): void {
  const text = updateErrorText(e);
  if (text !== null) ws().notify(text);
}

export function runUpdateAction(action: UpdateAction): void {
  switch (action.kind) {
    case "check":
      return void commands.updateCheck();
    case "download":
      return void downloadUpdate(action.version);
    case "install":
      return void installUpdate(action.version, null);
    default: {
      const unreachable: never = action;
      return unreachable;
    }
  }
}

async function downloadUpdate(version: string): Promise<void> {
  const res = await commands.updateDownload(version);
  if (res.status === "error") notifyUnlessStale(res.error);
}

const tabTitle = (id: TabId) => ws().tabs[id]?.title ?? "a closed tab";
const chatTitle = (id: ChatId) => useLive.getState().chats[id]?.title || "an untitled chat";

function tabsWithUnsubmittedEdits(): TabId[] {
  const { byTab } = useTableEdits.getState();
  return allCells(ws()).filter((id) => {
    const edits = byTab[id];
    return edits !== undefined && pendingCount(edits) > 0;
  });
}

type Accepted = { impact: RestartImpact; unsubmitted: TabId[] };

export async function installUpdate(version: string, accepted: Accepted | null): Promise<void> {
  const covered = accepted !== null && tabsWithUnsubmittedEdits().every((id) => accepted.unsubmitted.includes(id));
  if (covered) await flushWorkspaceSave();
  const res = await commands.updateInstall(version, covered ? accepted.impact : null);
  if (res.status === "error") return notifyUnlessStale(res.error);
  if (res.data.kind === "Restarting") return;
  const { impact } = res.data;
  const unsubmitted = tabsWithUnsubmittedEdits();
  const restart = await confirm({
    title: restartTitle(version, useLive.getState().updates),
    body: restartBody(impact, unsubmitted, tabTitle, chatTitle),
    confirmLabel: "Restart and install",
  });
  if (restart) await installUpdate(version, { impact, unsubmitted });
}

/** The select shows the engine's track, so a refused switch snaps back on its own. */
export async function chooseTrack(track: UpdateTrack): Promise<void> {
  const res = await commands.updateSetTrack(track);
  if (res.status === "error") notifyUnlessStale(res.error);
}

/** Failures never toast: a scheduled check on a laptop that is offline would toast every hour. */
export function startUpdateSync(): () => void {
  return useLive.subscribe((s, prev) => {
    const was = prev.updates?.status;
    const now = s.updates?.status;
    if (was?.kind === "Downloading" && now?.kind === "Downloaded") {
      ws().notify(`L1 Data ${now.version} is ready. Click the update button in the sidebar to restart into it.`);
    }
  });
}
