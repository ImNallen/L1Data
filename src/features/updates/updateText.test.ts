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

  it("is hidden while updates are off, and otherwise draws a glyph and says where the update is in one line", () => {
    const views = EVERY.map((s) => updateButtonView(s));
    expect(views.map((v) => v && [v.glyph, v.attention, v.title, v.detail, v.label])).toEqual([
      null,
      ["check", false, "Check for update", null, "Check for update"],
      ["spinner", false, "Checking for update…", null, "Checking for update"],
      ["check", false, "Check for update", "Up to date", "Check for update"],
      ["available", true, "0.2.0 is available", "Click to download", "Download L1 Data 0.2.0"],
      ["ring", false, "Downloading 0.2.0", "42%", "Downloading L1 Data 0.2.0"],
      ["restart", true, "0.2.0 is ready", "Click to restart", "Restart to install L1 Data 0.2.0"],
      ["spinner", false, "Restarting to install…", null, "Restarting"],
      ["error", false, "Couldn't reach GitHub for updates.", "Click to retry", "Update failed"],
    ]);
  });

  it("names an older stable as one, and draws an indeterminate ring with no percent when the size is unknown", () => {
    const older = updateButtonView({ kind: "Available", version: "0.1.0", downgrade: true, changes: [] });
    expect([older?.title, older?.detail]).toEqual(["Stable 0.1.0 is older than this build", "Click to download"]);
    const unknown = updateButtonView({ kind: "Downloading", version: "0.2.0", downgrade: false, percent: null, changes: [] });
    expect([unknown?.title, unknown?.detail, unknown?.percent]).toEqual(["Downloading 0.2.0", null, null]);
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

  it("labels each release by its version when several have changes, and credits only the PR", () => {
    expect(changeSections({ kind: "Available", version: "0.2.0", downgrade: false, changes })).toEqual([
      {
        version: "0.2.0",
        items: [
          { title: "feat(app): name the run target", pr: "#50" },
          { title: "fix(ui): cancel a cell edit on Escape", pr: "#51" },
        ],
      },
      {
        version: "0.1.1",
        items: [
          { title: "chore(ui): tighten the footer", pr: null },
          { title: "docs(readme): list the shortcuts", pr: null },
        ],
      },
    ]);
  });

  it("leaves a lone release with changes unlabeled", () => {
    const lone = changes.slice(0, 2);
    expect(changeSections({ kind: "Available", version: "0.2.0", downgrade: false, changes: lone })).toEqual([
      {
        version: null,
        items: [
          { title: "feat(app): name the run target", pr: "#50" },
          { title: "fix(ui): cancel a cell edit on Escape", pr: "#51" },
        ],
      },
    ]);
  });

  it("stays through the download", () => {
    const downloading = changeSections({ kind: "Downloading", version: "0.2.0", downgrade: false, percent: 10, changes });
    const downloaded = changeSections({ kind: "Downloaded", version: "0.2.0", downgrade: false, changes });
    expect([downloading.map((s) => s.version), downloaded.map((s) => s.version)]).toEqual([
      ["0.2.0", "0.1.1"],
      ["0.2.0", "0.1.1"],
    ]);
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
