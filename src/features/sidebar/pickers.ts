import type { Catalog, ConnectionConfig, DatabaseState, DbAccess, InstanceState, Preset, SchemaCount, ViewEdit } from "../../ipc/bindings";

/** `warn` paints the pill amber: the user chose to show nothing out of something. */
export type ShownCount = { shown: number; total: number; warn: boolean };

export type PickerItem = {
  name: string;
  checked: boolean;
  isDefault: boolean;
  trailing: string;
  trailingTitle?: string;
  disabled: boolean;
  system: boolean;
};

export type PickerModel = {
  title: string;
  context: string;
  count: ShownCount;
  placeholder: string;
  items: PickerItem[];
  footnote: string;
  systemLabel: string;
  showSystem: boolean;
  preset(p: Preset): ViewEdit;
  toggle(name: string, shown: boolean): ViewEdit;
  setShowSystem(show: boolean): ViewEdit;
};

function shownCount(shown: number, total: number): ShownCount {
  return { shown, total, warn: shown === 0 && total > 0 };
}

export function countText(c: ShownCount): string {
  return `${c.shown}/${c.total}`;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

const matches = (query: string) => {
  const q = query.trim().toLowerCase();
  return (name: string) => name.toLowerCase().includes(q);
};

const listedDatabases = (instance: InstanceState) => instance.databases.filter((d) => !d.system || instance.showSystemDatabases);

/** A database the user cannot connect to is listed but can never be shown, so it does not count. */
export function databaseCount(instance: InstanceState): ShownCount {
  const items = listedDatabases(instance).filter((d) => d.access === "Ok");
  return shownCount(items.filter((d) => d.shown).length, items.length);
}

const listedSchemas = (state: DatabaseState, catalog: Catalog) => catalog.schemas.filter((s) => !s.system || state.showSystemSchemas);

function countSchemas(state: DatabaseState, catalog: Catalog): ShownCount {
  const items = listedSchemas(state, catalog);
  const shown = new Set(state.shownSchemas);
  return shownCount(items.filter((s) => shown.has(s.name)).length, items.length);
}

export function schemaCount(state: DatabaseState, catalog: Catalog | undefined): ShownCount | null {
  return catalog ? countSchemas(state, catalog) : null;
}

const ACCESS_REASON: Record<Exclude<DbAccess, "Ok">, string> = {
  NoConnections: "Connections are not allowed",
  NoPrivilege: "No CONNECT privilege",
};

function schemaCountText(count: SchemaCount): Pick<PickerItem, "trailing" | "trailingTitle"> {
  switch (count.kind) {
    case "Known":
      return { trailing: plural(count.count, "schema", "schemas") };
    case "Unknown":
    case "Probing":
      return { trailing: "…" };
    case "Unavailable":
      return { trailing: "No access", trailingTitle: count.message };
    default: {
      const unreachable: never = count;
      return unreachable;
    }
  }
}

function databaseItem(d: DatabaseState, home: string): PickerItem {
  const base = { name: d.name, checked: d.shown, isDefault: d.name === home, system: d.system };
  if (d.access !== "Ok") return { ...base, trailing: "No access", trailingTitle: ACCESS_REASON[d.access], disabled: true };
  return { ...base, ...schemaCountText(d.schemaCount), disabled: false };
}

export function databasePicker(config: ConnectionConfig, instance: InstanceState, query: string): PickerModel {
  const count = databaseCount(instance);
  const match = matches(query);
  return {
    title: "Databases in sidebar",
    context: config.name,
    count,
    placeholder: "Search databases…",
    items: listedDatabases(instance)
      .filter((d) => match(d.name))
      .map((d) => databaseItem(d, instance.home)),
    footnote: "Changes save automatically. Hiding a database keeps its notebooks and queries open.",
    systemLabel: "Show system databases",
    showSystem: instance.showSystemDatabases,
    preset: (preset) => ({ kind: "Databases", preset }),
    toggle: (name, shown) => ({ kind: "Database", name, shown }),
    setShowSystem: (show) => ({ kind: "ShowSystemDatabases", show }),
  };
}

export function schemaPicker(state: DatabaseState, catalog: Catalog, query: string, connectionName: string): PickerModel {
  const database = state.name;
  const count = countSchemas(state, catalog);
  const shown = new Set(state.shownSchemas);
  const match = matches(query);
  return {
    title: "Schemas in sidebar",
    context: `${database} · ${connectionName}`,
    count,
    placeholder: "Search schemas…",
    items: listedSchemas(state, catalog)
      .filter((s) => match(s.name))
      .map((s) => ({
        name: s.name,
        checked: shown.has(s.name),
        isDefault: s.name === catalog.defaultSchema,
        trailing: plural(s.tableCount, "table", "tables"),
        disabled: false,
        system: s.system,
      })),
    footnote: "Changes save automatically for this database. Hiding a schema keeps its queries open.",
    systemLabel: "Show system schemas",
    showSystem: state.showSystemSchemas,
    preset: (preset) => ({ kind: "Schemas", database, preset }),
    toggle: (name, shown) => ({ kind: "Schema", database, name, shown }),
    setShowSystem: (show) => ({ kind: "ShowSystemSchemas", database, show }),
  };
}

/** `anchor` is the CSS edge the box hangs from: `top` under the row, `bottom` above it. */
type Box = { anchor: { top: number } | { bottom: number }; left: number; width: number; maxHeight: number };

/** Header, filter, presets, footer, and a few rows. Below this much room the picker goes to the roomier side. */
const PICKER_FITS = 240;

export function pickerBox(
  row: { left: number; top: number; bottom: number; width: number },
  viewport: { width: number; height: number },
  contentHeight = PICKER_FITS,
): Box {
  const width = Math.max(0, Math.min(360, viewport.width - 16));
  const right = row.left + row.width + 4;
  const beside = right + width <= viewport.width - 8;
  const left = Math.max(8, Math.min(beside ? right : row.left, viewport.width - 8 - width));
  if (beside) {
    const top = Math.max(8, Math.min(row.top, viewport.height - contentHeight - 8));
    return { anchor: { top }, left, width, maxHeight: Math.max(0, viewport.height - top - 8) };
  }
  const below = viewport.height - row.bottom - 12;
  const above = row.top - 12;
  if (below >= PICKER_FITS || below >= above) {
    const top = Math.max(8, Math.min(row.bottom + 4, viewport.height - 8));
    return { anchor: { top }, left, width, maxHeight: Math.max(0, viewport.height - top - 8) };
  }
  const bottom = Math.max(8, Math.min(viewport.height - row.top + 4, viewport.height - 8));
  return { anchor: { bottom }, left, width, maxHeight: Math.max(0, viewport.height - bottom - 8) };
}

/** The rows on screen whose schema count nobody has asked for yet. */
export function namesToProbe(instance: InstanceState, visible: readonly PickerItem[]): string[] {
  const counts = new Map(instance.databases.map((d) => [d.name, d.schemaCount.kind]));
  return visible.filter((i) => !i.disabled && counts.get(i.name) === "Unknown").map((i) => i.name);
}
