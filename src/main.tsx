import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "./index.css";

import { isTauri } from "@tauri-apps/api/core";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { boot } from "./boot";
import { AppLayout } from "./shell/AppLayout";
import { BootStatus } from "./shell/BootStatus";

/** `mounting` paints the app under the opaque splash so the reveal never shows a half-drawn first frame. */
type Phase = "starting" | "mounting" | "revealing" | "ready";

function startupTree(phase: Phase, onLeft: () => void) {
  return (
    <>
      {phase !== "starting" && (
        <StrictMode key="app">
          <AppLayout />
        </StrictMode>
      )}
      {phase !== "ready" && <BootStatus key="splash" state={{ kind: "starting" }} leaving={phase === "revealing"} onLeft={onLeft} />}
    </>
  );
}

const nextPaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

async function start() {
  const root = document.getElementById("root");
  if (!root) throw new Error("index.html is missing #root");
  const reactRoot = createRoot(root);
  const show = (phase: Phase) => reactRoot.render(startupTree(phase, () => show("ready")));
  const bridge = (import.meta.env.DEV || import.meta.env.MODE === "bridge") && !isTauri();
  show("starting");
  try {
    if (bridge) {
      const { installBridgeShim } = await import("./dev/bridgeShim");
      installBridgeShim();
    }
    await boot();
  } catch (e) {
    reactRoot.render(<BootStatus state={{ kind: "failed", error: String(e), bridge, retry: () => window.location.reload() }} />);
    return;
  }
  show("mounting");
  await nextPaint();
  await document.fonts.ready;
  show("revealing");
}

void start();
