import { matchRanges } from "../../ui/textMatch";

// Plain data, so a setting's component can read its own label without importing the registry that imports it.

type IndexedSection = {
  id: string;
  label: string;
  /** `group` is the card a setting sits in, shared by its neighbours; null draws it bare. */
  settings: readonly { id: string; label: string; group: string | null; keywords: string }[];
};

export const SETTINGS_INDEX = [
  {
    id: "general",
    label: "General",
    settings: [
      {
        id: "assistantAccess",
        label: "Assistant access",
        group: "Assistant",
        keywords: "ai privacy schema only full access data contents rows attachments editor context global chat",
      },
      { id: "maxRows", label: "Row limit per result", group: "Results", keywords: "queries max rows limit results" },
      { id: "tablePageSize", label: "Results page size", group: "Results", keywords: "queries table rows per page paging browse" },
      { id: "stopOnError", label: "Stop a script at the first error", group: "Scripts", keywords: "queries errors script abort fail skip" },
      { id: "appVersion", label: "Version", group: "Updates", keywords: "about version update check download install restart release" },
      { id: "updateTrack", label: "Update track", group: "Updates", keywords: "about update release channel track stable nightly downgrade switch" },
    ],
  },
  {
    id: "appearance",
    label: "Appearance",
    settings: [
      { id: "colorScheme", label: "Color scheme", group: null, keywords: "appearance dark light system mode colour color" },
      {
        id: "themes",
        label: "Themes",
        group: null,
        keywords: "theme palette colours colors accent custom create import vs code open vsx dracula catppuccin nord tokyo night",
      },
      { id: "density", label: "Density", group: "Layout", keywords: "compact standard spacious spacing padding row height comfortable dense layout" },
      {
        id: "reduceMotion",
        label: "Reduce motion",
        group: "Motion",
        keywords: "motion animation accessibility reduced prefers system movement slide fade pulse vestibular",
      },
      {
        id: "panelMotion",
        label: "Panel animations",
        group: "Motion",
        keywords: "motion animation speed duration reduce transitions sidebar panels open close",
      },
      {
        id: "tintConnections",
        label: "Tint connections by color",
        group: "Sidebar",
        keywords: "connection colour color tint background sidebar tree dot highlight production",
      },
      { id: "interfaceFont", label: "Interface font", group: "Typography", keywords: "typography font family size text ui system sf pro inter zoom" },
      {
        id: "codeFont",
        label: "Monospace font",
        group: "Typography",
        keywords: "typography font family size code editor table grid results mono jetbrains sf mono menlo",
      },
    ],
  },
  {
    id: "providers",
    label: "Providers",
    settings: [
      {
        id: "providerList",
        label: "Providers",
        group: null,
        keywords: "ai assistant claude cli binary path config directory launch arguments sign in plan version model",
      },
      { id: "providerHealthCheck", label: "Health check interval", group: "Advanced", keywords: "refresh status version models background interval seconds" },
    ],
  },
] as const satisfies readonly IndexedSection[];

export type SettingsSectionId = (typeof SETTINGS_INDEX)[number]["id"];
export type SettingId = (typeof SETTINGS_INDEX)[number]["settings"][number]["id"];

export type SettingEntry = { id: SettingId; label: string; group: string | null; keywords: string; section: SettingsSectionId; sectionLabel: string };

export const SETTINGS: readonly SettingEntry[] = SETTINGS_INDEX.flatMap(({ id: section, label: sectionLabel, settings }) =>
  settings.map((s) => ({ ...s, section, sectionLabel })),
);

const LABELS = new Map(SETTINGS.map((s) => [s.id, s.label]));

export function settingLabel(id: SettingId): string {
  return LABELS.get(id) ?? id;
}

export function sectionLabel(id: SettingsSectionId): string {
  return SETTINGS_INDEX.find((s) => s.id === id)?.label ?? id;
}

export function sectionSettings(id: SettingsSectionId): readonly SettingEntry[] {
  return SETTINGS.filter((s) => s.section === id);
}

/** Consecutive settings that share a section and a group: one card, or one bare stretch when the group is null. */
export function settingRuns(settings: readonly SettingEntry[]): { section: SettingsSectionId; group: string | null; settings: SettingEntry[] }[] {
  const runs: { section: SettingsSectionId; group: string | null; settings: SettingEntry[] }[] = [];
  for (const s of settings) {
    const last = runs.at(-1);
    if (last && last.section === s.section && last.group === s.group) last.settings.push(s);
    else runs.push({ section: s.section, group: s.group, settings: [s] });
  }
  return runs;
}

/** The settings whose label, section, group, and keywords hold every token, in registry order. */
export function filterSettings(tokens: readonly string[]): readonly SettingEntry[] {
  return SETTINGS.filter((s) => matchRanges(tokens, `${s.label} ${s.sectionLabel} ${s.group ?? ""} ${s.keywords}`) !== null);
}
