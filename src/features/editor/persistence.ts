import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { commands } from "../../ipc/bindings";
import { useWorkspace, workspaceSnapshot } from "../../state/workspace";
import { isDirty, onBuffersChanged, textOf } from "./editorStates";

const SAVE_DEBOUNCE_MS = 500;

/** The webview's share of a window close. The shell destroys the window 5 s after the close request. */
const CLOSE_FLUSH_MS = 2_000;

let timer: ReturnType<typeof setTimeout> | undefined;

const save = () => commands.workspaceSave(workspaceSnapshot(useWorkspace.getState(), { textOf, isDirty }));

/** A background tab's reload from disk and a save's new baseline change no store field, so buffer changes schedule too. */
export function startPersistence(): void {
  const schedule = () => {
    clearTimeout(timer);
    timer = setTimeout(() => void save(), SAVE_DEBOUNCE_MS);
  };
  useWorkspace.subscribe((s, prev) => {
    if (s.tabs !== prev.tabs || s.notebooks !== prev.notebooks || s.shown !== prev.shown || s.layout !== prev.layout) schedule();
  });
  onBuffersChanged(schedule);
}

/** Writes the pending save now. The snapshot is taken before this returns its promise. */
export async function flushWorkspaceSave(): Promise<void> {
  clearTimeout(timer);
  await save();
}

export async function flushBeforeClose(savesInFlight: () => Promise<void>): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const bound = new Promise<void>((resolve) => (timer = setTimeout(resolve, CLOSE_FLUSH_MS)));
  await Promise.race([Promise.allSettled([flushWorkspaceSave(), Promise.resolve().then(savesInFlight)]), bound]);
  clearTimeout(timer);
}

/** Tauri holds a window's close while a close-requested listener exists, and destroys the window once it resolves. */
export function installCloseFlush(savesInFlight: () => Promise<void>): void {
  if (!isTauri()) return;
  void getCurrentWindow().onCloseRequested(() => flushBeforeClose(savesInFlight));
}
