import type {
  ColumnInfo,
  ConnectionConfig,
  ConnectionId,
  ConnectionState,
  DatabaseRef,
  ExtensionInfo,
  FunctionInfo,
  Relation,
  SequenceInfo,
  SynonymInfo,
  TypeInfo,
} from "../../ipc/bindings";
import { isViewKind, viewFor, type CatalogView } from "../../state/catalog";
import type { DbKey } from "../../state/dbKey";
import { instanceOf } from "../../state/live";
import type { RelationRef } from "../../state/workspace";
import { matchRanges, startsWords, type TextRange } from "../../ui/textMatch";

export type ObjectFilterScope = { db: DatabaseRef; category: "tables" | "views" | "all" };

export type FilteredColumn = { column: ColumnInfo; ranges: TextRange[] };
/** `ranges` is null when the relation shows only as the parent of a matching column. */
export type FilteredRelation = { rel: Relation; ranges: TextRange[] | null; columns: FilteredColumn[] };
export type FilteredFunction = { fn: FunctionInfo; ranges: TextRange[] };
export type FilteredSequence = { seq: SequenceInfo; ranges: TextRange[] };
export type FilteredType = { type: TypeInfo; ranges: TextRange[] };
export type FilteredSynonym = { syn: SynonymInfo; ranges: TextRange[] };
export type FilteredExtension = { ext: ExtensionInfo; ranges: TextRange[] };
export type FilteredSchema = {
  name: string;
  relations: FilteredRelation[];
  functions: FilteredFunction[];
  sequences: FilteredSequence[];
  types: FilteredType[];
  synonyms: FilteredSynonym[];
};
export type FilteredDatabase = { db: DatabaseRef; schemas: FilteredSchema[]; extensions: FilteredExtension[] };
export type FilteredConnection = { config: ConnectionConfig; databases: FilteredDatabase[] };
/** `count` is the matched objects. `searched` is how many catalogs the filter could search. */
export type TreeFilter = { connections: FilteredConnection[]; count: number; searched: number };

function filterRelation(tokens: readonly string[], rel: Relation): FilteredRelation | null {
  const ranges = matchRanges(tokens, rel.name);
  const columns = rel.columns.flatMap((column) => {
    const r = matchRanges(tokens, column.name);
    return r ? [{ column, ranges: r }] : [];
  });
  return ranges || columns.length > 0 ? { rel, ranges, columns } : null;
}

/**
 * The tree's working set narrowed to the objects whose own name holds every token: the shown schemas of
 * the shown databases of connected instances, wherever the catalog is loaded. Containers never match
 * themselves; they stay only as ancestors of a match. Order follows the tree.
 */
export function filterTree(
  tokens: readonly string[],
  configs: Record<ConnectionId, ConnectionConfig>,
  connections: Record<ConnectionId, ConnectionState>,
  views: Record<DbKey, CatalogView>,
  scope?: ObjectFilterScope | null,
): TreeFilter {
  let count = 0;
  let searched = 0;
  const extras = !scope || scope.category === "all";
  const filtered = Object.values(configs)
    .filter((config) => !scope || config.id === scope.db.connectionId)
    .sort((a, b) => a.name.localeCompare(b.name))
    .flatMap((config): FilteredConnection[] => {
      const shown = instanceOf(connections[config.id])?.databases.filter((d) => d.shown && (!scope || d.name === scope.db.database)) ?? [];
      const databases = shown.flatMap((state): FilteredDatabase[] => {
        const db = { connectionId: config.id, database: state.name };
        const view = viewFor(views, db);
        if (!view) return [];
        searched++;
        const schemas = state.shownSchemas.flatMap((name): FilteredSchema[] => {
          const schema = view.catalog.schemas.find((s) => s.name === name);
          if (!schema) return [];
          const ordered = [...schema.relations.filter((r) => !isViewKind(r.kind)), ...schema.relations.filter((r) => isViewKind(r.kind))];
          const relations = ordered
            .filter((rel) => extras || isViewKind(rel.kind) === (scope?.category === "views"))
            .flatMap((rel) => filterRelation(tokens, rel) ?? []);
          const functions = (extras ? schema.functions : []).flatMap((fn) => {
            const ranges = matchRanges(tokens, fn.name);
            return ranges ? [{ fn, ranges }] : [];
          });
          const sequences = (extras ? schema.sequences : []).flatMap((seq) => {
            const ranges = matchRanges(tokens, seq.name);
            return ranges ? [{ seq, ranges }] : [];
          });
          const types = (extras ? schema.types : []).flatMap((type) => {
            const ranges = matchRanges(tokens, type.name);
            return ranges ? [{ type, ranges }] : [];
          });
          const synonyms = (extras ? schema.synonyms : []).flatMap((syn) => {
            const ranges = matchRanges(tokens, syn.name);
            return ranges ? [{ syn, ranges }] : [];
          });
          count +=
            relations.reduce((n, r) => n + (r.ranges ? 1 : 0) + r.columns.length, 0) + functions.length + sequences.length + types.length + synonyms.length;
          return relations.length > 0 || functions.length > 0 || sequences.length > 0 || types.length > 0 || synonyms.length > 0
            ? [{ name, relations, functions, sequences, types, synonyms }]
            : [];
        });
        const extensions = (extras ? view.catalog.extensions : []).flatMap((ext) => {
          const ranges = matchRanges(tokens, ext.name);
          return ranges ? [{ ext, ranges }] : [];
        });
        count += extensions.length;
        return schemas.length > 0 || extensions.length > 0 ? [{ db, schemas, extensions }] : [];
      });
      return databases.length > 0 ? [{ config, databases }] : [];
    });
  return { connections: filtered, count, searched };
}

/**
 * The table or view Enter in the search box opens: the first the filtered tree lists whose name has every
 * token at a word start, else the first it lists. "ord it" then opens `order_items`, not `order_audit`.
 */
export function firstRelation(filter: TreeFilter, tokens: readonly string[]): RelationRef | null {
  const listed = filter.connections.flatMap(({ databases }) =>
    databases.flatMap(({ db, schemas }) => schemas.flatMap((s) => s.relations.map(({ rel }) => ({ db, schema: s.name, relation: rel.name })))),
  );
  return listed.find((r) => startsWords(tokens, r.relation)) ?? listed[0] ?? null;
}
