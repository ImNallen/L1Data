import { describe, expect, it } from "vitest";
import type { RestartImpact, UpdateState, UpdateStatus } from "../../ipc/bindings";
import { aboutStatusText, actionLabel, changeSections, restartBody, restartTitle, updateAction, updateButtonView } from "./updateText";

const NOW = 10 * 60_000;
const TAB_A = "a0000000-0000-4000-8000-00000000000a";
const TAB_B = "b0000000-0000-4000-8000-00000000000b";
const CHAT = "c0000000-0000-4000-8000-00000000000c";

const state = (status: UpdateStatus, currentVersion = "0.1.0"): UpdateState => ({
  rev: 1,
  track: "Stable",
  currentVersion,
  status,
  checkedAtMs: NOW - 5 * 60_000,
});

const EVERY: UpdateStatus[] = [
  { kind: "Disabled", reason: "DevBuild" },
  { kind: "Idle" },
  { kind: "Checking" },
  { kind: "UpToDate" },
  { kind: "Available", version: "0.2.0", downgrade: false, changes: [] },
  { kind: "Downloading", version: "0.2.0", downgrade: false, percent: 42, changes: [] },
  { kind: "Downloaded", version: "0.2.0", downgrade: false, changes: [] },
  { kind: "Installing", version: "0.2.0" },
  { kind: "Failed", failure: { kind: "Offline" } },
];

describe("the update button", () => {
  it("takes the next step from where the update is, and does nothing while a step runs", () => {
    expect(EVERY.map((s) => [s.kind, updateAction(s)])).toEqual([
      ["Disabled", null],
      ["Idle", { kind: "check" }],
      ["Checking", null],
      ["UpToDate", { kind: "check" }],
      ["Available", { kind: "download", version: "0.2.0" }],
      ["Downloading", null],
      ["Downloaded", { kind: "install", version: "0.2.0" }],
      ["Installing", null],
      ["Failed", { kind: "check" }],
    ]);
  });

  it("is hidden while updates are off, and otherwise draws a glyph and explains itself", () => {
    const views = EVERY.map((s) => updateButtonView(state(s), NOW));
    expect(views.map((v) => v && [v.glyph, v.attention, v.tooltip])).toEqual([
      null,
      ["check", false, "L1 Data 0.1.0"],
      ["spinner", false, "Checking for updates…"],
      ["check", false, "L1 Data 0.1.0 is up to date. Checked 5m ago. Click to check again."],
      ["available", true, "Update available · Click the icon to download"],
      ["ring", false, "Downloading… 42%"],
      ["restart", true, "Ready to install · Restart required"],
      ["spinner", false, "Restarting to install…"],
      ["error", false, "Couldn't reach GitHub for updates. Click to try again."],
    ]);
  });

  it("names an older stable as one, and draws an indeterminate ring when the size is unknown", () => {
    const nightly = "0.1.1-nightly.20261023.5";
    const older = updateButtonView(state({ kind: "Available", version: "0.1.0", downgrade: true, changes: [] }, nightly), NOW);
    expect(older?.heading).toBe("L1 Data 0.1.0");
    expect(older?.tooltip).toBe("Update available · Click the icon to download. It is the latest Stable build, older than this nightly.");
    const unknown = updateButtonView(state({ kind: "Downloading", version: "0.2.0", downgrade: false, percent: null, changes: [] }), NOW);
    expect([unknown?.heading, unknown?.percent, unknown?.tooltip]).toEqual(["L1 Data 0.2.0", null, "Downloading…"]);
  });
});

describe("the update's change list", () => {
  const changes = [
    {
      version: "0.2.0",
      items: [
        { title: "feat(app): name the run target", author: "ImNallen", pr: 50 },
        { title: "fix(ui): cancel a cell edit on Escape", author: null, pr: 51 },
      ],
    },
    { version: "0.1.2", items: [] },
    {
      version: "0.1.1",
      items: [
        { title: "chore(ui): tighten the footer", author: "ImNallen", pr: null },
        { title: "docs(readme): list the shortcuts", author: null, pr: null },
      ],
    },
  ];

  it("heads the offered release as what changed and each skipped one by its version, and credits what the feed names", () => {
    expect(changeSections({ kind: "Available", version: "0.2.0", downgrade: false, changes })).toEqual([
      {
        heading: "What's changed",
        items: [
          { title: "feat(app): name the run target", credit: "@ImNallen · #50" },
          { title: "fix(ui): cancel a cell edit on Escape", credit: "#51" },
        ],
      },
      {
        heading: "Changes in 0.1.1",
        items: [
          { title: "chore(ui): tighten the footer", credit: "@ImNallen" },
          { title: "docs(readme): list the shortcuts", credit: null },
        ],
      },
    ]);
  });

  it("stays through the download, and never says what changed about a release the feed did not list", () => {
    const downloading = changeSections({ kind: "Downloading", version: "0.2.0", downgrade: false, percent: 10, changes });
    const downloaded = changeSections({ kind: "Downloaded", version: "0.2.0", downgrade: false, changes });
    expect([downloading.length, downloaded.length]).toEqual([2, 2]);
    const skipped = changeSections({ kind: "Available", version: "0.2.1", downgrade: false, changes });
    expect(skipped.map((s) => s.heading)).toEqual(["Changes in 0.2.0", "Changes in 0.1.1"]);
  });

  it("has nothing to show in any other status", () => {
    const others = EVERY.filter((s) => !["Available", "Downloading", "Downloaded"].includes(s.kind));
    expect(others.map(changeSections)).toEqual(others.map(() => []));
  });
});

describe("Settings > General", () => {
  it("says where the update is and names the same step as the sidebar button", () => {
    expect(EVERY.map((s) => [aboutStatusText(state(s), NOW), actionLabel(s)])).toEqual([
      ["Development builds don't update themselves.", "Check for updates"],
      ["Not checked yet.", "Check for updates"],
      ["Checking for updates…", "Checking…"],
      ["Up to date. Checked 5m ago.", "Check for updates"],
      ["0.2.0 is available.", "Download 0.2.0"],
      ["Downloading 0.2.0… 42%", "Downloading…"],
      ["0.2.0 is ready to install.", "Restart to install"],
      ["Restarting into 0.2.0…", "Restarting…"],
      ["Couldn't reach GitHub for updates.", "Check for updates"],
    ]);
  });
});

describe("the restart dialog", () => {
  const titles: Record<string, string> = { [TAB_A]: "orders.sql", [TAB_B]: "fix-prices.sql", [CHAT]: "Top customers" };
  const body = (impact: Partial<RestartImpact>, staged: string[] = []) =>
    restartBody(
      { running: [], transactions: [], chats: [], ...impact },
      staged,
      (id) => titles[id] ?? id,
      (id) => titles[id] ?? id,
    );

  it("says only that the app reopens when nothing would be interrupted", () => {
    expect(body({})).toBe("L1 Data will close and reopen.");
  });

  it("names every tab and chat the restart interrupts", () => {
    expect(body({ running: [TAB_A], transactions: [{ tabId: TAB_B, hasWrites: true }], chats: [CHAT] })).toBe(
      "Restarting cancels the query running in “orders.sql”, rolls back the uncommitted changes in “fix-prices.sql”, and stops the assistant in “Top customers”. L1 Data will close and reopen.",
    );
    expect(body({ running: [TAB_A, TAB_B] })).toBe(
      "Restarting cancels the queries running in “orders.sql” and “fix-prices.sql”. L1 Data will close and reopen.",
    );
    expect(body({ transactions: [{ tabId: TAB_A, hasWrites: false }] })).toBe(
      "Restarting ends the open transaction in “orders.sql”. L1 Data will close and reopen.",
    );
    expect(body({}, [TAB_A])).toBe("Restarting discards the changes in “orders.sql” that haven't been submitted. L1 Data will close and reopen.");
    expect(body({ running: [TAB_A] }, [TAB_A, TAB_B])).toBe(
      "Restarting cancels the query running in “orders.sql” and discards the changes in “orders.sql” and “fix-prices.sql” that haven't been submitted. L1 Data will close and reopen.",
    );
  });

  it("calls an older build one in the title", () => {
    expect(restartTitle("0.2.0", state({ kind: "Downloaded", version: "0.2.0", downgrade: false, changes: [] }))).toBe("Restart to install L1 Data 0.2.0?");
    expect(restartTitle("0.1.0", state({ kind: "Downloaded", version: "0.1.0", downgrade: true, changes: [] }))).toBe(
      "Restart to install L1 Data 0.1.0, an older build?",
    );
  });
});
