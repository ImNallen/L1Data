import { createElement, Suspense, use, useEffect, useState, type ComponentType } from "react";

// Rarely opened surfaces load from their own chunks, so the main chunk skips their code and their libraries:
// fflate and jsonc-parser behind Settings, @pierre/words behind the assistant. Each loads once
// the app idles after first paint, so the first open usually finds it ready. A surface opened before then renders
// nothing until its chunk arrives. Once loaded, a surface renders synchronously, with no fallback frame.
function lazySurface(load: () => Promise<ComponentType>) {
  let loaded: ComponentType | null = null;
  let pending: Promise<ComponentType> | null = null;
  const preload = () =>
    (pending ??= load().then(
      (component) => (loaded = component),
      (error: unknown) => {
        pending = null;
        throw error;
      },
    ));
  // The loaded component is one module-level value, so this never creates a component during render.
  const Loaded = () => createElement(loaded ?? use(preload()));
  function Surface() {
    return (
      <Suspense fallback={null}>
        <Loaded />
      </Suspense>
    );
  }
  return { Surface, preload, isLoaded: () => loaded !== null };
}

const settings = lazySurface(() => import("../features/settings/SettingsScreen").then((m) => m.SettingsDialog));
const connection = lazySurface(() => import("../features/connections/ConnectionDialog").then((m) => m.ConnectionDialog));
const assistant = lazySurface(() => import("../features/assistant/AssistantPanel").then((m) => m.AssistantPanel));
const SURFACES = [settings, connection, assistant];

export const SettingsDialog = settings.Surface;
export const ConnectionDialog = connection.Surface;
export const AssistantPanel = assistant.Surface;

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
