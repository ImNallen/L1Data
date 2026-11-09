import type { DatabaseRef, TabId } from "../../ipc/bindings";
import { findRelation, isViewKind, type CatalogView } from "../../state/catalog";
import { dbKey, homeDb, type DbKey } from "../../state/dbKey";
import { instanceOf, type LiveData } from "../../state/live";
import { notebookFor } from "../../state/notebooks";
import { activeId, allCells, type RelationRef, type WorkspaceData } from "../../state/workspace";
import { parseQuery, startsWords } from "../../ui/textMatch";

export type Destination = { kind: "tab"; tabId: TabId } | { kind: "database"; db: DatabaseRef } | { kind: "relation"; ref: RelationRef };

export type NavigationItem = {
  id: string;
  label: string;
  context: string;
  kindLabel: string;
  action: string;
  destination: Destination;
};

export type NavigationSource = Pick<WorkspaceData, "tabs" | "notebooks" | "shown" | "configs"> &
  Pick<LiveData, "connections"> & { views: Record<DbKey, CatalogView> };

export function navigationItems(source: NavigationSource): NavigationItem[] {
  const items: NavigationItem[] = [];
  const connectionName = (db: DatabaseRef) => source.configs[db.connectionId]?.name ?? db.connectionId;
  const active = activeId(source);
  for (const id of allCells(source)) {
    const tab = source.tabs[id];
    if (!tab) continue;
    const db = tab.console.db;
    const relation = tab.table && db && findRelation(source.views[dbKey(db)], tab.table.schema, tab.table.table);
    const kindLabel = tab.table ? (relation && isViewKind(relation.kind) ? "View tab" : "Table tab") : "Query tab";
    items.push({
      id: JSON.stringify(["tab", id]),
      label: tab.title,
      context: [db ? `${connectionName(db)} / ${db.database}` : "No database", tab.table?.schema].filter(Boolean).join(" / "),
      kindLabel,
      action: active === id ? "Focus active tab" : "Switch to tab",
      destination: { kind: "tab", tabId: id },
    });
  }
  for (const config of Object.values(source.configs).sort((a, b) => a.name.localeCompare(b.name))) {
    const instance = instanceOf(source.connections[config.id]);
    const databases = instance ? instance.databases.filter((db) => db.access === "Ok").map((db) => db.name) : [homeDb(config).database];
    for (const database of databases) {
      const db = { connectionId: config.id, database };
      const notebook = notebookFor(source.notebooks, db);
      items.push({
        id: JSON.stringify(["database", config.id, database]),
        label: database,
        context: `${config.name} / ${instance ? "Connected" : "Disconnected · saved home database"}`,
        kindLabel: "Database",
        action: notebook ? `Switch to the ${notebook.title} notebook` : "Open new query in this database",
        destination: { kind: "database", db },
      });
      const view = source.views[dbKey(db)];
      if (!instance || !view) continue;
      for (const schema of view.catalog.schemas) {
        for (const relation of schema.relations) {
          const kindLabel = relation.kind === "MaterializedView" ? "Materialized view" : isViewKind(relation.kind) ? "View" : "Table";
          items.push({
            id: JSON.stringify(["relation", config.id, database, schema.name, relation.name]),
            label: relation.name,
            context: `${config.name} / ${database} / ${schema.name}`,
            kindLabel,
            action: `Open ${kindLabel.toLowerCase()}`,
            destination: { kind: "relation", ref: { db, schema: schema.name, relation: relation.name } },
          });
        }
      }
    }
  }
  return items;
}

export function searchNavigation(items: readonly NavigationItem[], query: string): { items: NavigationItem[]; total: number } {
  const tokens = parseQuery(query);
  const phrase = tokens.join(" ");
  const ranked = items.flatMap((item, index) => {
    const name = item.label.toLowerCase();
    const searchable = `${name} ${item.context} ${item.kindLabel}`.toLowerCase();
    if (!tokens.every((token) => searchable.includes(token))) return [];
    const nameTokens = tokens.filter((token) => name.includes(token));
    const rank = !tokens.length
      ? 0
      : name === phrase
        ? 0
        : name.startsWith(phrase)
          ? 1
          : startsWords(tokens, name)
            ? 2
            : nameTokens.length === tokens.length
              ? 3
              : 4;
    return [{ item, rank, nameMatches: nameTokens.length, index }];
  });
  ranked.sort((a, b) => a.rank - b.rank || b.nameMatches - a.nameMatches || a.index - b.index);
  return { items: ranked.slice(0, 60).map(({ item }) => item), total: ranked.length };
}
