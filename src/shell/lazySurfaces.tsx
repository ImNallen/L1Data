import { createElement, Suspense, use, useEffect, useState, type ComponentType } from "react";

// Rarely opened surfaces load from their own chunks once the app idles after first paint, so the main chunk skips
// their libraries. A surface opened before its chunk arrives renders nothing.
function lazySurface<P extends object>(load: () => Promise<ComponentType<P>>) {
  let loaded: ComponentType<P> | null = null;
  let pending: Promise<ComponentType<P>> | null = null;
  const preload = () =>
    (pending ??= load().then(
      (component) => (loaded = component),
      (error: unknown) => {
        pending = null;
        throw error;
      },
    ));
  // The loaded component is one module-level value, so this never creates a component during render.
  const Loaded = (props: P) => createElement(loaded ?? use(preload()), props);
  function Surface(props: P) {
    return (
      <Suspense fallback={null}>
        <Loaded {...props} />
      </Suspense>
    );
  }
  return { Surface, preload, isLoaded: () => loaded !== null };
}

const settings = lazySurface(() => import("../features/settings/SettingsScreen").then((m) => m.SettingsDialog));
const connection = lazySurface(() => import("../features/connections/ConnectionDialog").then((m) => m.ConnectionDialog));
const assistant = lazySurface(() => import("../features/assistant/AssistantPanel").then((m) => m.AssistantPanel));
const replyText = lazySurface(() => import("../features/assistant/ReplyText").then((m) => m.ReplyText));
const SURFACES = [settings, connection, assistant, replyText];

export const SettingsDialog = settings.Surface;
export const ConnectionDialog = connection.Surface;
export const AssistantPanel = assistant.Surface;
export const ReplyText = replyText.Surface;

/** Loads every surface's chunk. Tests await it so a surface renders in the same pass that opens it. */
export const preloadSurfaces = () => Promise.all(SURFACES.map((s) => s.preload())).then(() => undefined);

function whenIdle(run: () => void): () => void {
  if (typeof window.requestIdleCallback === "function") {
    const id = window.requestIdleCallback(run, { timeout: 2000 });
    return () => window.cancelIdleCallback(id);
  }
  // WKWebView has no requestIdleCallback.
  const id = window.setTimeout(run, 200);
  return () => window.clearTimeout(id);
}

/** True once every surface has loaded after the first idle period. */
export function useSurfacesReady(): boolean {
  const [ready, setReady] = useState(() => SURFACES.every((s) => s.isLoaded()));
  useEffect(() => {
    if (ready) return;
    let live = true;
    const cancel = whenIdle(() => {
      preloadSurfaces().then(
        () => live && setReady(true),
        () => {},
      );
    });
    return () => {
      live = false;
      cancel();
    };
  }, [ready]);
  return ready;
}

/**
 * Mounts a dialog once its chunk is ready or the first time it opens, then keeps it mounted,
 * so its close animation and focus return run as they would for a dialog that was always there.
 */
export function useMountOnce(open: boolean, ready: boolean): boolean {
  const [mounted, setMounted] = useState(open || ready);
  if ((open || ready) && !mounted) setMounted(true);
  return mounted;
}
