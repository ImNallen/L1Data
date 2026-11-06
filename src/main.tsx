import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "./index.css";

import { isTauri } from "@tauri-apps/api/core";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { boot } from "./boot";
import { AppLayout } from "./shell/AppLayout";
import { BootStatus } from "./shell/BootStatus";

async function start() {
  const root = document.getElementById("root");
  if (!root) throw new Error("index.html is missing #root");
  const reactRoot = createRoot(root);
  const bridge = (import.meta.env.DEV || import.meta.env.MODE === "bridge") && !isTauri();
  reactRoot.render(<BootStatus state={{ kind: "starting" }} />);
  try {
    if (bridge) {
      const { installBridgeShim } = await import("./dev/bridgeShim");
      installBridgeShim();
    }
    await boot();
    reactRoot.render(
      <StrictMode>
        <AppLayout />
      </StrictMode>,
    );
  } catch (e) {
    reactRoot.render(<BootStatus state={{ kind: "failed", error: String(e), bridge, retry: () => window.location.reload() }} />);
  }
}

void start();
