import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { Sidebar } from "../features/sidebar/Sidebar";
import type { ConnectionConfig, TxnMode } from "../ipc/bindings";
import { EMPTY_QUERY, useWorkspace, type Tab } from "../state/workspace";
import { AppLayout } from "./AppLayout";
import { preloadSurfaces } from "./lazySurfaces";
import { RightPanel } from "./RightPanel";
import { ResizeEdges, WindowControlButtons } from "./WindowChrome";
import { TopBar } from "./TopBar";
import { hostedWindow, type CustomChrome } from "./hostedWindow";

const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
const WINDOWS = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
const LINUX = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36";

const fakeHost: CustomChrome = {
  chrome: "custom",
  minimize: async () => {},
  toggleMaximize: async () => {},
  close: async () => {},
  maximized: async () => false,
  onResize: async () => () => {},
  startResize: async () => {},
};

function inTauri(userAgent: string) {
  vi.stubGlobal("isTauri", true);
  vi.stubGlobal("window", { __TAURI_INTERNALS__: { metadata: { currentWindow: { label: "main" } } } });
  vi.stubGlobal("navigator", { userAgent });
}

const { layout: initialLayout, tabs: initialTabs, active: initialActive, configs: initialConfigs, order: initialOrder } = useWorkspace.getInitialState();

beforeAll(preloadSurfaces);

afterEach(() => {
  vi.unstubAllGlobals();
  Object.assign(useWorkspace.getInitialState(), {
    layout: initialLayout,
    tabs: initialTabs,
    active: initialActive,
    configs: initialConfigs,
    order: initialOrder,
  });
});

// Static rendering reads zustand's server snapshot, which is the store's initial state, so setState can't reach it.
function withLayout(patch: Partial<typeof initialLayout>) {
  Object.assign(useWorkspace.getInitialState(), { layout: { ...initialLayout, ...patch } });
}

const controlOrder = (html: string) => html.match(/data-testid="window-(?:minimize|maximize|close)"/g);

it("lets the window be dragged from the sidebar header", () => {
  expect(renderToStaticMarkup(<Sidebar hosted={null} />)).toContain('data-tauri-drag-region="deep"');
});

it("leaves macOS to its native traffic lights and resizing", () => {
  inTauri(MAC);
  expect(hostedWindow(MAC)).toEqual({ chrome: "native" });

  const topBar = renderToStaticMarkup(<TopBar hosted={hostedWindow(MAC)} />);
  expect(topBar).toContain('data-tauri-drag-region="deep"');
  expect(topBar).not.toContain('data-testid="window-controls"');

  const layout = renderToStaticMarkup(<AppLayout />);
  expect(layout).not.toContain("window-resize-");
  expect(layout).not.toContain('data-testid="window-controls"');
});

it.each([
  ["Windows", WINDOWS],
  ["Linux", LINUX],
])("draws minimize, maximize, and close at the right of the toolbar on %s", (_os, userAgent) => {
  inTauri(userAgent);
  const hosted = hostedWindow(userAgent);
  expect(hosted?.chrome).toBe("custom");

  const topBar = renderToStaticMarkup(<TopBar hosted={hosted} />);
  expect(controlOrder(topBar)).toEqual(['data-testid="window-minimize"', 'data-testid="window-maximize"', 'data-testid="window-close"']);
  expect(topBar.indexOf('data-testid="toggle-right-panel"')).toBeLessThan(topBar.indexOf('data-testid="window-controls"'));

  expect(renderToStaticMarkup(<AppLayout />)).toContain('data-testid="window-resize-North"');
});

it("has no hosted window in the browser, so the toolbar stays draggable with no window buttons", () => {
  expect(hostedWindow(MAC)).toBeNull();
  expect(hostedWindow(LINUX)).toBeNull();

  const html = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(html).toContain('data-tauri-drag-region="deep"');
  expect(html).not.toContain('data-testid="window-controls"');
});

it("labels the maximize button Restore once the window is maximized", () => {
  const restored = renderToStaticMarkup(<WindowControlButtons hosted={fakeHost} maximized={false} />);
  expect(restored).toContain('aria-label="Maximize"');
  expect(restored).toContain("hover:bg-danger");

  const maximized = renderToStaticMarkup(<WindowControlButtons hosted={fakeHost} maximized={true} />);
  expect(controlOrder(maximized)).toEqual(['data-testid="window-minimize"', 'data-testid="window-maximize"', 'data-testid="window-close"']);
  expect(maximized).toContain('aria-label="Restore"');
});

it("gives the frameless window a resize edge on every side and corner", () => {
  const html = renderToStaticMarkup(<ResizeEdges hosted={fakeHost} />);
  expect(html.match(/data-testid="window-resize-[A-Za-z]+"/g)).toEqual([
    'data-testid="window-resize-North"',
    'data-testid="window-resize-South"',
    'data-testid="window-resize-West"',
    'data-testid="window-resize-East"',
    'data-testid="window-resize-NorthWest"',
    'data-testid="window-resize-NorthEast"',
    'data-testid="window-resize-SouthWest"',
    'data-testid="window-resize-SouthEast"',
  ]);
});

const TOGGLE = 'data-testid="toggle-right-panel"';
const CONTROLS = 'data-testid="window-controls"';
const RESULTS = 'data-testid="toggle-results"';

it("keeps the right-panel toggle in the top bar while the panel is closed", () => {
  withLayout({ rightPanel: null });
  const topBar = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(topBar).toContain(TOGGLE);
  expect(topBar.indexOf(RESULTS)).toBeLessThan(topBar.indexOf(TOGGLE));
});

it("moves the right-panel toggle into the open panel's header in place of a close button", () => {
  withLayout({ rightPanel: "Assistant" });
  const topBar = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(topBar).not.toContain(TOGGLE);
  expect(topBar).not.toContain(RESULTS);

  const panel = renderToStaticMarkup(<RightPanel hosted={null} />);
  expect(panel).toContain(TOGGLE);
  expect(panel.indexOf(RESULTS)).toBeLessThan(panel.indexOf(TOGGLE));
  expect(panel).not.toContain('aria-label="Close panel"');
  expect(panel).toContain('data-tauri-drag-region="deep"');
});

it("heads the right panel with Assistant, new chat, and chat history before the panel toggles, over the chat composer", () => {
  withLayout({ rightPanel: "Assistant" });
  const panel = renderToStaticMarkup(<RightPanel hosted={null} />);
  expect(panel).toMatch(/<aside[^>]*aria-labelledby="right-panel-title".*<h2 id="right-panel-title"[^>]*>Assistant<\/h2>/);
  const order = ['data-testid="assistant-new"', 'data-testid="assistant-history"', TOGGLE, 'data-testid="assistant-panel"'].map((id) => panel.indexOf(id));
  expect(order.every((at, i) => at > (order[i - 1] ?? 0))).toBe(true);
  expect(panel).toMatch(/<textarea[^>]*data-testid="assistant-input"[^>]*placeholder="Message assistant…"/);
  expect(panel).not.toContain("coming soon");
});

it("insets the toggle from the window edge by the same amount in either header", () => {
  for (const hosted of [null, { chrome: "native" } as const, fakeHost]) {
    withLayout({ rightPanel: null });
    const topBarInset = renderToStaticMarkup(<TopBar hosted={hosted} />).match(/<header[^>]*class="[^"]*\b(pr-\d)\b/)?.[1];
    withLayout({ rightPanel: "Assistant" });
    const panelInset = renderToStaticMarkup(<RightPanel hosted={hosted} />).match(/data-tauri-drag-region="deep" class="[^"]*\b(pr-\d)\b/)?.[1];
    expect(panelInset).toBe(topBarInset);
    expect(topBarInset).toBe(hosted?.chrome === "custom" ? "pr-2" : "pr-3");
  }
});

it("keeps the custom window controls after the toggle in whichever header holds it", () => {
  withLayout({ rightPanel: null });
  const closed = renderToStaticMarkup(<TopBar hosted={fakeHost} />);
  expect(closed).toContain(CONTROLS);
  expect(closed.indexOf(TOGGLE)).toBeLessThan(closed.indexOf(CONTROLS));

  withLayout({ rightPanel: "Assistant" });
  expect(renderToStaticMarkup(<TopBar hosted={fakeHost} />)).not.toContain(CONTROLS);
  const open = renderToStaticMarkup(<RightPanel hosted={fakeHost} />);
  expect(controlOrder(open)).toEqual(['data-testid="window-minimize"', 'data-testid="window-maximize"', 'data-testid="window-close"']);
  expect(open.indexOf(TOGGLE)).toBeLessThan(open.indexOf(CONTROLS));
});

const SIDEBAR_TOGGLE = 'data-testid="toggle-sidebar"';
const leadingInset = (html: string) => html.match(/data-tauri-drag-region="deep" class="[^"]*\b(pl-\S+?)(?=[\s"])/)?.[1];

it("puts the sidebar toggle before the app title while the sidebar is open, and keeps it out of the top bar", () => {
  withLayout({ sidebar: true });
  const sidebar = renderToStaticMarkup(<Sidebar hosted={null} />);
  expect(sidebar).toContain(SIDEBAR_TOGGLE);
  expect(sidebar.indexOf(SIDEBAR_TOGGLE)).toBeLessThan(sidebar.indexOf('data-testid="new-connection"'));
  expect(renderToStaticMarkup(<TopBar hosted={null} />)).not.toContain(SIDEBAR_TOGGLE);
});

it("leads the top bar with the sidebar toggle once the sidebar is hidden", () => {
  withLayout({ sidebar: false });
  const topBar = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(topBar.match(/<(?:button|a|input)\b[^>]*/)?.[0]).toContain(SIDEBAR_TOGGLE);
});

it("insets the sidebar toggle from the window's left edge by the same amount in either header", () => {
  for (const hosted of [null, { chrome: "native" } as const, fakeHost]) {
    withLayout({ sidebar: true });
    const sidebarInset = leadingInset(renderToStaticMarkup(<Sidebar hosted={hosted} />));
    withLayout({ sidebar: false });
    const topBarInset = leadingInset(renderToStaticMarkup(<TopBar hosted={hosted} />));
    expect(topBarInset).toBe(sidebarInset);
    expect(topBarInset).toBe(hosted?.chrome === "native" ? "pl-[86px]" : "pl-3");
  }
});

// An empty element: the line itself is the handle, with no pill inside it.
const handle = (html: string, id: string) => html.match(new RegExp(`<div[^>]*data-testid="${id}"[^>]*></div>`))?.[0];

it("puts a labelled resize line on the open sidebar's edge that shows hover, focus, and drag", () => {
  withLayout({ sidebar: true });
  const html = renderToStaticMarkup(<AppLayout />);
  const separator = handle(html, "sidebar-resize");
  expect(separator).toContain('role="separator"');
  expect(separator).toContain('aria-label="Resize sidebar"');
  for (const state of ["hover:bg-handle-hover", "focus-visible:bg-focus", "data-[separator=active]:bg-run"]) expect(separator).toContain(state);
  expect(html.indexOf('data-testid="sidebar-header"')).toBeLessThan(html.indexOf('data-testid="sidebar-resize"'));
});

it("drops the sidebar resize handle while the sidebar is hidden", () => {
  withLayout({ sidebar: false });
  const html = renderToStaticMarkup(<AppLayout />);
  expect(html).not.toContain('data-testid="sidebar-resize"');
  expect(html).not.toContain('aria-label="Resize sidebar"');
});

function withQueryTab() {
  const tab: Tab = { id: "t1", title: "Untitled-1.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null };
  Object.assign(useWorkspace.getInitialState(), { tabs: { t1: tab }, order: ["t1"], active: "t1" });
}

it("keeps an unrun cell compact and its results directly below it", () => {
  withQueryTab();
  withLayout({ results: true });
  const html = renderToStaticMarkup(<AppLayout />);
  expect(html).toContain('data-testid="notebook-cell-t1"');
  expect(html).toContain('data-testid="sql-editor"');
  expect(html).not.toContain('data-testid="results-resize"');
  expect(html).not.toContain("Results appear here");
});

it("keeps the editor while results are hidden", () => {
  withQueryTab();
  withLayout({ results: false });
  const html = renderToStaticMarkup(<AppLayout />);
  expect(html).toContain('data-testid="sql-editor"');
  expect(html).not.toContain('class="notebook-results"');
});

it("keeps the empty notebook's start commands while results are hidden", () => {
  withLayout({ results: false });
  const html = renderToStaticMarkup(<AppLayout />);
  expect(html).toContain('data-testid="no-tabs"');
  expect(html).not.toContain("Results appear here");
});

it("lists the start commands with their shortcuts when no tab is open", () => {
  const html = renderToStaticMarkup(<AppLayout />);
  const rows = [...html.matchAll(/data-testid="start-[^"]+"[^>]*>([^<]+)<kbd[^>]*>([^<]+)<\/kbd>/g)].map((m) => [m[1], m[2]]);
  expect(rows).toEqual([
    ["New query tab", "Ctrl+T"],
    ["Open file…", "Ctrl+O"],
    ["New connection", "Ctrl+N"],
    ["Quick navigation", "Ctrl+P"],
    ["Saved queries", "Ctrl+E"],
    ["Settings", "Ctrl+,"],
  ]);
  expect(html).not.toContain("No open tabs.");
});

it("makes the open right panel a resizable panel after the main area, behind its own resize line", () => {
  withLayout({ rightPanel: "Assistant" });
  const html = renderToStaticMarkup(<AppLayout />);
  const right = handle(html, "right-panel-resize");
  expect(right).toContain('role="separator"');
  expect(right).toContain('aria-label="Resize right panel"');
  const order = ['data-testid="main"', 'data-testid="right-panel-resize"', 'data-testid="right"', 'data-testid="right-panel"'].map((m) => html.indexOf(m));
  expect(order.every((at) => at >= 0)).toBe(true);
  expect(order).toEqual([...order].sort((a, b) => a - b));
});

it("drops the right panel and its handle while the panel is closed", () => {
  withLayout({ rightPanel: null });
  const html = renderToStaticMarkup(<AppLayout />);
  expect(html).not.toContain('data-testid="right-panel-resize"');
  expect(html).not.toContain('aria-label="Resize right panel"');
  expect(html).not.toContain('data-testid="right-panel"');
});

function withTableTab(mode: TxnMode = "AutoCommit", color: string | null = null) {
  const tab: Tab = {
    id: "t2",
    title: "orders",
    console: { db: { connectionId: "c", database: "shop" }, mode },
    table: { schema: "public", table: "orders", page: 0, query: EMPTY_QUERY },
    file: null,
  };
  const config: ConnectionConfig = {
    id: "c",
    name: "dev",
    host: "localhost",
    port: 5433,
    database: "shop",
    user: "l1data",
    sslMode: "Prefer",
    caCertPath: null,
    databaseKind: "Postgres",
    hasPassword: true,
    color,
    ssh: null,
    environment: null,
    sqlServerAuth: null,
  };
  Object.assign(useWorkspace.getInitialState(), { tabs: { t2: tab }, order: ["t2"], active: "t2", configs: { c: config } });
}

it("shows the transaction mode and cursor position on a table tab", () => {
  withTableTab();
  withLayout({ results: true });
  const html = renderToStaticMarkup(<AppLayout />);
  expect(html).toMatch(/data-testid="tx-mode"[^>]*>Auto-commit</);
  expect(html).toContain('data-testid="notebook-cell-t2"');
});

it("gives a table tab the connection, database, and transaction mode menus a query tab has", () => {
  withTableTab("Manual");
  const html = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(html).toMatch(/<button[^>]*aria-haspopup="menu"[^>]*data-testid="tab-connection"[^>]*><span class="truncate">dev</);
  expect(html).toMatch(/<button[^>]*aria-haspopup="menu"[^>]*data-testid="tx-mode"[^>]*>Manual</);
  expect(html).toMatch(/<button[^>]*aria-haspopup="menu"[^>]*data-testid="tab-database"[^>]*><span class="truncate">shop</);
});

it("marks the tab's connection with its color in the top bar", () => {
  withTableTab("AutoCommit", "red");
  const html = renderToStaticMarkup(<TopBar hosted={null} />);
  expect(html).toMatch(
    /data-testid="tab-connection"[^>]*><span aria-hidden="true" class="[^"]*" style="background-color:#ef4444"><\/span><span class="truncate">dev</,
  );
});
