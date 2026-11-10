import { openDatabaseMap } from "../databaseMap/controller";
import { beginImport } from "../results/importState";
import {
  BracesIcon,
  CheckCheckIcon,
  CircleSmallIcon,
  ClockIcon,
  DatabaseIcon,
  EllipsisIcon,
  SquareArrowOutUpRightIcon,
  EqualIcon,
  EyeIcon,
  HashIcon,
  KeyIcon,
  LinkIcon,
  ListFilterIcon,
  ListIcon,
  SigmaIcon,
  SquareFunctionIcon,
  Table2Icon,
  ToggleLeftIcon,
  TypeIcon,
  ZapIcon,
  type LucideIcon,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type MouseEvent, type ReactNode, type RefObject } from "react";
import { useShallow } from "zustand/react/shallow";
import {
  commands,
  type ColumnInfo,
  type ConnectionConfig,
  type ConnectionId,
  type ConnectionState,
  type DatabaseRef,
  type DatabaseState,
  type FunctionInfo,
  type InstanceState,
  type KeyKind,
  type Relation,
  type RelationDetail,
  type SchemaInfo,
  type ScriptTarget,
  type TypeCategory,
  type TypeKind,
} from "../../ipc/bindings";
import { isViewKind, useCatalog, viewFor, type CatalogView } from "../../state/catalog";
import { connectErrorText, detailErrorText } from "../../ipc/errors";
import { DATABASE_LABEL, databaseKindFor } from "../../state/databaseKind";
import { dbKey, sameDb, type DbKey } from "../../state/dbKey";
import { instanceOf, useLive } from "../../state/live";
import { detailKey, useRelationDetail, type DetailEntry } from "../../state/relationDetail";
import { connectionTone, notebookActivity, type Tone } from "../../state/status";
import { activeTab, tabTarget, useWorkspace, type RelationRef } from "../../state/workspace";
import { ContextMenuItem, ContextMenuSeparator, ContextSubmenu, Highlight, Spinner, StatusDot, cx } from "../../ui/primitives";
import { matchRanges, parseQuery, type TextRange } from "../../ui/textMatch";
import { connect, confirmDelete, disconnect, refreshCatalog, refreshConnection } from "../connections/connectionActions";
import { ConnectionColorDot, EnvironmentBadge, EnvironmentIcon, connectionColor } from "../connections/connectionColors";
import { insertAtCursor } from "../editor/editorStates";
import { openSqlTab, openTableTab } from "../editor/runActions";
import { columnType, defaultText, indexText, keyText, triggerText, type RowText } from "./detailText";
import { databaseCount, schemaCount } from "./pickers";
import type { FilteredRelation, FilteredSchema, TreeFilter } from "./treeFilter";
import { PickerPill, type PickerTarget } from "./VisibilityPopover";
import { INDENT, VirtualTree, type RowNode, type TintGroup, type TreeNode } from "./VirtualTree";
import type { ShownCount } from "./pickers";

type Expanded = { has(path: string): boolean; toggle(path: string, open?: boolean): void };

/**
 * What the builders below need to flatten the tree into rows. `out` collects the rows in the order the user sees them.
 * `details` and `idle` collect the fetches the open rows ask for, which the tree starts once the rows are built.
 */
type Build = {
  out: TreeNode[];
  expanded: Expanded;
  tint: TintGroup | null;
  selected: RelationRef | null;
  entries: Record<string, DetailEntry>;
  details: { db: DatabaseRef; version: number; oid: number }[];
  idle: DatabaseRef[];
  browse: { selected: DatabaseRef | null; select(db: DatabaseRef): void } | null;
  /** How many objects a schema holds in the shown category, for a filtered schema's "6 of 17". */
  total: ((db: DatabaseRef, schema: string) => number | null) | null;
};

function build(expanded: Expanded, selected: RelationRef | null = null, entries: Record<string, DetailEntry> = {}): Build {
  return { out: [], expanded, tint: null, selected, entries, details: [], idle: [], browse: null, total: null };
}

function row(b: Build, node: Omit<RowNode, "kind" | "tint">): void {
  b.out.push({
    kind: "row",
    tint: b.tint,
    ...node,
    view: (active) => {
      const view = node.view(active);
      const qualified = (node.path ?? node.key)
        .split("/")
        .filter((part) => !part.startsWith("#"))
        .join(" / ");
      return { ...view, title: view.title && view.title !== node.name ? `${qualified} · ${view.title}` : qualified };
    },
  });
}

function note(b: Build, key: string, depth: number, size: number, render: () => ReactNode): void {
  b.out.push({ kind: "note", key, depth, tint: b.tint, size, render });
}

function NewMenu({ db }: { db: DatabaseRef }) {
  return (
    <ContextSubmenu label="New" testId="tree-menu-new">
      <ContextMenuItem testId="tree-menu-query-console" onSelect={() => openSqlTab({ db })}>
        Query Console
      </ContextMenuItem>
    </ContextSubmenu>
  );
}

export async function openObjectScript(db: DatabaseRef, target: ScriptTarget): Promise<void> {
  const res = await commands.objectScript(db, target);
  if (res.status === "error") return useWorkspace.getState().notify(detailErrorText(res.error));
  openSqlTab({ db, text: res.data.text, title: res.data.title });
  if (res.data.warnings.length > 0) useWorkspace.getState().notify(`L1 Data could not script: ${res.data.warnings.join("; ")}.`);
}

function ScriptMenuItem({ db, target, label }: { db: DatabaseRef; target: ScriptTarget; label: string }) {
  return (
    <ContextMenuItem
      testId={label === "Script as CREATE" ? "tree-menu-script-create" : "tree-menu-open-source"}
      onSelect={() => void openObjectScript(db, target)}
    >
      {label}
    </ContextMenuItem>
  );
}

function RelationMenu({ db, schema, rel }: { db: DatabaseRef; schema: string; rel: Relation }) {
  return (
    <>
      <ScriptMenuItem db={db} target={{ kind: "Relation", oid: rel.oid }} label="Script as CREATE" />
      <ContextMenuItem testId="tree-menu-database-map" onSelect={() => openDatabaseMap(db, rel.oid)}>
        Explore database map
      </ContextMenuItem>
      {(rel.kind === "Table" || rel.kind === "PartitionedTable") && (
        <ContextMenuItem testId="tree-menu-import" onSelect={() => void beginImport({ db, schema, table: rel.name, oid: rel.oid })}>
          Import CSV…
        </ContextMenuItem>
      )}
      <ContextMenuSeparator />
      <NewMenu db={db} />
    </>
  );
}

/** A schema reads as a heading over its objects. */
function SchemaName({ name }: { name: string }) {
  return <span className="text-ui-11.5 font-semibold">{name}</span>;
}

function Count({ children }: { children: ReactNode }) {
  return <span className="shrink-0 pl-2 text-ui-10.5 font-normal text-dim tabular-nums">{children}</span>;
}

const COLUMN_GLYPHS: Partial<Record<TypeCategory, LucideIcon>> = {
  Integer: HashIcon,
  Numeric: SigmaIcon,
  Text: TypeIcon,
  Enum: ListIcon,
  Bool: ToggleLeftIcon,
  Json: BracesIcon,
  Temporal: ClockIcon,
};

const glyph = (Glyph: LucideIcon, className = "text-dim") => <Glyph size={14} className={className} />;

function DetailName({
  name,
  title,
  marks,
  detail,
  nameClass = "font-mono text-ui-12",
}: {
  name: ReactNode;
  title: string;
  marks?: ReactNode;
  detail?: string;
  nameClass?: string;
}) {
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span title={title} className={cx("min-w-0 truncate", nameClass)}>
        {name}
      </span>
      {marks}
      {detail !== undefined && (
        <span title={detail} className="min-w-0 flex-1 basis-0 truncate pl-2 text-right font-mono text-ui-11 text-dim">
          {detail}
        </span>
      )}
    </span>
  );
}

function columnRow(b: Build, path: string, depth: number, column: ColumnInfo, ranges?: readonly TextRange[]) {
  row(b, {
    key: `${path}/${column.name}`,
    depth,
    name: column.name,
    expandable: false,
    keepChevronSpace: true,
    view: () => ({
      icon: glyph(COLUMN_GLYPHS[column.category] ?? CircleSmallIcon),
      label: (
        <DetailName
          name={<Highlight text={column.name} ranges={ranges} />}
          title={column.name}
          marks={
            <>
              {column.pk && <KeyIcon size={13} className="shrink-0 text-amber" />}
              {column.fk && <LinkIcon size={13} className="shrink-0 text-badge" />}
            </>
          }
          detail={columnType(column)}
        />
      ),
    }),
  });
}

const KEY_GLYPHS: Record<KeyKind["kind"], ReactNode> = {
  Primary: glyph(KeyIcon, "text-amber"),
  Foreign: glyph(LinkIcon, "text-badge"),
  Unique: glyph(KeyIcon),
  Check: glyph(CheckCheckIcon),
};

function textRow(b: Build, key: string, depth: number, icon: ReactNode, name: string, text: RowText, menu?: () => ReactNode) {
  row(b, {
    key,
    depth,
    name,
    expandable: false,
    keepChevronSpace: true,
    twoLines: text.second !== undefined,
    menu,
    view: () => ({ icon, label: <DetailName name={name} title={name} detail={text.trailing} />, second: text.second }),
  });
}

/** A row that opens and closes the rows `children` adds under it. `count` is null while it loads. */
function group(b: Build, path: string, depth: number, label: string, count: number | null, icon: ReactNode | undefined, children: () => void) {
  const open = b.expanded.has(path);
  row(b, {
    key: path,
    depth,
    name: label,
    expandable: true,
    open,
    onClick: () => b.expanded.toggle(path),
    view: () => ({
      icon,
      label,
      trailing: <Count>{count ?? <span className="text-dim">…</span>}</Count>,
    }),
  });
  if (open) children();
}

type DetailGroup = { id: string; label: string; count: number | null; rows: () => void };

/** Columns come from the catalog. The other groups wait for `relation_detail` and hide when empty. */
function detailGroups(b: Build, path: string, depth: number, db: DatabaseRef, schema: string, rel: Relation, detail: RelationDetail | null): DetailGroup[] {
  const at = (group: string, name: string) => `${path}/#${group}/${name}`;
  const columns: DetailGroup = {
    id: "columns",
    label: "Columns",
    count: rel.columns.length,
    rows: () => rel.columns.forEach((c) => columnRow(b, `${path}/#columns`, depth, c)),
  };
  if (!detail) {
    const pending = (id: string, label: string): DetailGroup => ({ id, label, count: null, rows: () => {} });
    return [columns, pending("defaults", "Defaults"), pending("keys", "Keys"), pending("indexes", "Indexes"), pending("triggers", "Triggers")];
  }
  const rest: DetailGroup[] = [
    {
      id: "defaults",
      label: "Defaults",
      count: detail.defaults.length,
      rows: () =>
        detail.defaults.forEach((d) =>
          row(b, {
            key: at("defaults", d.column),
            depth,
            name: d.column,
            expandable: false,
            keepChevronSpace: true,
            view: () => ({ icon: glyph(EqualIcon), label: <DetailName name={d.column} title={d.column} detail={defaultText(d.value)} /> }),
          }),
        ),
    },
    {
      id: "keys",
      label: "Keys",
      count: detail.keys.length,
      rows: () => detail.keys.forEach((k) => textRow(b, at("keys", k.name), depth, KEY_GLYPHS[k.kind.kind], k.name, keyText(k, schema))),
    },
    {
      id: "indexes",
      label: "Indexes",
      count: detail.indexes.length,
      rows: () =>
        detail.indexes.forEach((i) =>
          textRow(b, at("indexes", i.name), depth, glyph(ListFilterIcon), i.name, indexText(i), () => (
            <ScriptMenuItem db={db} target={{ kind: "Index", relationOid: rel.oid, name: i.name }} label="Open source" />
          )),
        ),
    },
    {
      id: "triggers",
      label: "Triggers",
      count: detail.triggers.length,
      rows: () =>
        detail.triggers.forEach((t) =>
          textRow(b, at("triggers", t.name), depth, glyph(ZapIcon), t.name, triggerText(t), () => (
            <ScriptMenuItem db={db} target={{ kind: "Trigger", relationOid: rel.oid, name: t.name }} label="Open source" />
          )),
        ),
    },
  ];
  return [columns, ...rest.filter((g) => g.count !== 0)];
}

/** An open relation's groups. The tree fetches its detail on first open and again whenever the catalog version moves. */
function relationGroups(b: Build, path: string, depth: number, db: DatabaseRef, schema: string, rel: Relation, version: number) {
  b.details.push({ db, version, oid: rel.oid });
  const entry = b.entries[detailKey(db, rel.oid)];
  for (const g of detailGroups(b, path, depth + 1, db, schema, rel, entry?.detail ?? null)) {
    group(b, `${path}/#${g.id}`, depth, g.label, g.count, undefined, g.rows);
  }
  const error = entry?.error;
  if (error) {
    note(b, `${path}/!detail-error`, depth, 20, () => (
      <div data-testid={`detail-error-${path}`} style={{ paddingLeft: 30 + depth * INDENT }} className="mb-1 pr-2 text-ui-11.5 leading-4 text-danger/90">
        {error}
      </div>
    ));
  }
}

const isSelected = (selected: RelationRef | null, db: DatabaseRef, schema: string, rel: Relation) =>
  sameDb(selected?.db, db) && selected?.schema === schema && selected.relation === rel.name;

const relationIcon = (rel: Relation, selected: boolean) => {
  const Icon = isViewKind(rel.kind) ? EyeIcon : Table2Icon;
  return <Icon size={14} className={selected ? "text-keyword" : "text-dim"} />;
};

const COMPACT = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });

/** A table's row estimate at rest, and Open and the row's menu while it is hovered or focused. */
function RelationTrailing({ db, schema, rel }: { db: DatabaseRef; schema: string; rel: Relation }) {
  const rows = rel.rowEstimate;
  return (
    <Swap
      held={false}
      rest={
        rows !== null && (
          <span title={`About ${rows.toLocaleString("en")} rows`} className="pl-2 text-ui-10.5 font-normal text-dim tabular-nums">
            {COMPACT.format(rows).replace("K", "k")}
          </span>
        )
      }
      control={
        <span className="flex items-center gap-0.5 pl-1">
          <RowButton label={`Open ${rel.name}`} onClick={() => openTableTab({ db, schema, relation: rel.name })}>
            <SquareArrowOutUpRightIcon size={13} />
          </RowButton>
          <RowMenuButton label={`${rel.name} options`} />
        </span>
      }
    />
  );
}

const relationActions = (db: DatabaseRef, schema: string, rel: Relation) => ({
  onClick: () => useWorkspace.getState().select({ db, schema, relation: rel.name }),
  onDoubleClick: () => openTableTab({ db, schema, relation: rel.name }),
  menu: () => <RelationMenu db={db} schema={schema} rel={rel} />,
});

function RelationName({ name, ranges }: { name: string; ranges?: readonly TextRange[] | null }) {
  if (name.length <= 12) return <Highlight text={name} ranges={ranges} />;
  const split = Math.max(name.length - 12, Math.floor(name.length / 2));
  const before = ranges?.filter((r) => r.start < split).map((r) => ({ start: r.start, end: Math.min(r.end, split) }));
  const after = ranges?.filter((r) => r.end > split).map((r) => ({ start: Math.max(0, r.start - split), end: r.end - split }));
  return (
    <span className="flex min-w-0">
      <span className="sr-only">{name}</span>
      <span aria-hidden className="min-w-0 truncate">
        <Highlight text={name.slice(0, split)} ranges={before} />
      </span>
      <span aria-hidden dir="rtl" className="max-w-1/2 shrink-0 overflow-hidden text-left whitespace-nowrap">
        <bdi>
          <Highlight text={name.slice(split)} ranges={after} />
        </bdi>
      </span>
    </span>
  );
}

function relationRow(b: Build, path: string, depth: number, db: DatabaseRef, schema: string, rel: Relation, version: number) {
  const me = `${path}/${rel.name}`;
  const open = b.expanded.has(me);
  row(b, {
    key: me,
    depth,
    name: rel.name,
    expandable: true,
    quietChevron: true,
    open,
    selected: isSelected(b.selected, db, schema, rel),
    ...relationActions(db, schema, rel),
    onToggle: () => {
      if (!open) b.expanded.toggle(`${me}/#columns`, true);
      b.expanded.toggle(me);
    },
    view: () => {
      const selected = isSelected(b.selected, db, schema, rel);
      return {
        icon: relationIcon(rel, selected),
        label: (
          <span className={cx("block min-w-0", selected && "font-medium")}>
            <RelationName name={rel.name} />
          </span>
        ),
        trailing: <RelationTrailing db={db} schema={schema} rel={rel} />,
      };
    },
  });
  if (open) relationGroups(b, me, depth + 1, db, schema, rel, version);
}

/** Inserts the routine's call template at the active tab's cursor, when that tab's database is of the routine's kind. */
export function insertRoutine(db: DatabaseRef, fn: FunctionInfo): void {
  const workspace = useWorkspace.getState();
  const { configs, notify } = workspace;
  const active = activeTab(workspace);
  const target = tabTarget(active);
  if (!active || !target || !configs[target.connectionId]) {
    notify("Choose a tab with a saved connection before inserting a routine.");
    return;
  }
  if (!configs[db.connectionId]) {
    notify("The routine's connection is no longer available.");
    return;
  }
  const sourceKind = databaseKindFor(db, configs);
  if (sourceKind !== databaseKindFor(target, configs)) {
    notify(`Choose a ${DATABASE_LABEL[sourceKind]} tab to insert this routine.`);
    return;
  }
  insertAtCursor(active.id, fn.insertText);
}

function functionRow(b: Build, path: string, depth: number, db: DatabaseRef, fn: FunctionInfo, ranges?: readonly TextRange[]) {
  row(b, {
    key: `${path}/${fn.name}#${fn.oid}`,
    path: `${path}/${fn.name}`,
    depth,
    name: fn.name,
    expandable: false,
    keepChevronSpace: true,
    menu: () => <ScriptMenuItem db={db} target={{ kind: "Routine", oid: fn.oid }} label="Open source" />,
    onDoubleClick: () => insertRoutine(db, fn),
    view: () => ({
      icon: glyph(SquareFunctionIcon),
      label: (
        <DetailName name={<Highlight text={fn.name} ranges={ranges} />} title={`${fn.name}(${fn.args}) → ${fn.returns}`} detail={fn.returns} nameClass="" />
      ),
    }),
  });
}

function typeDetail(kind: TypeKind): string | undefined {
  switch (kind.kind) {
    case "Enum":
      return kind.labels.join(", ") || undefined;
    case "Domain":
      return kind.base || undefined;
    case "Composite":
      return kind.fields.map((field) => `${field.name} ${field.typeName}`).join(", ") || undefined;
    case "Range":
      return kind.subtype || undefined;
    case "Builtin":
      return undefined;
    case "Alias":
      return `${kind.base}, ${kind.nullable ? "null" : "not null"}`;
    case "Table":
      return "table";
  }
}

type ObjectRow = { oid: number; name: string; detail?: string; ranges?: readonly TextRange[]; target: ScriptTarget };

function objectGroup(b: Build, path: string, depth: number, db: DatabaseRef, label: string, icon: LucideIcon, rows: ObjectRow[]) {
  if (rows.length === 0) return;
  group(b, path, depth, label, rows.length, glyph(icon), () => {
    for (const r of rows) {
      const title = <Highlight text={r.name} ranges={r.ranges} />;
      row(b, {
        key: `${path}/${r.name}`,
        depth: depth + 1,
        name: r.name,
        expandable: false,
        keepChevronSpace: true,
        menu: () => <ScriptMenuItem db={db} target={r.target} label="Script as CREATE" />,
        view: () => ({
          icon: glyph(icon),
          label: r.detail ? <DetailName name={title} title={`${r.name} ${r.detail}`} detail={r.detail} nameClass="" /> : title,
        }),
      });
    }
  });
}

function schemaRows(b: Build, path: string, depth: number, db: DatabaseRef, schema: SchemaInfo, version: number) {
  const me = `${path}/${schema.name}`;
  const open = b.expanded.has(me);
  row(b, {
    key: me,
    depth,
    name: schema.name,
    expandable: true,
    open,
    strong: true,
    height: 28,
    onClick: () => b.expanded.toggle(me),
    menu: () => <NewMenu db={db} />,
    view: () => ({ label: <SchemaName name={schema.name} />, trailing: <Count>{schema.tableCount}</Count> }),
  });
  if (!open) return;
  const tables = schema.relations.filter((r) => !isViewKind(r.kind));
  const views = schema.relations.filter((r) => isViewKind(r.kind));
  for (const rel of [...tables, ...views]) relationRow(b, me, depth + 1, db, schema.name, rel, version);
  if (schema.functions.length > 0) {
    const functions = `${me}/#functions`;
    group(b, functions, depth + 1, "Functions", schema.functions.length, <BracesIcon size={14} className="text-muted" />, () =>
      schema.functions.forEach((fn) => functionRow(b, functions, depth + 2, db, fn)),
    );
  }
  objectGroup(
    b,
    `${me}/#sequences`,
    depth + 1,
    db,
    "Sequences",
    HashIcon,
    schema.sequences.map((seq) => ({ oid: seq.oid, name: seq.name, detail: seq.typeName, target: { kind: "Sequence", oid: seq.oid } })),
  );
  objectGroup(
    b,
    `${me}/#types`,
    depth + 1,
    db,
    "Types",
    TypeIcon,
    schema.types.map((type) => ({ oid: type.oid, name: type.name, detail: typeDetail(type.kind), target: { kind: "Type", oid: type.oid } })),
  );
  objectGroup(
    b,
    `${me}/#synonyms`,
    depth + 1,
    db,
    "Synonyms",
    LinkIcon,
    schema.synonyms.map((syn) => ({ oid: syn.oid, name: syn.name, detail: syn.target, target: { kind: "Synonym", oid: syn.oid } })),
  );
}

/** One schema's rows in the unfiltered tree. */
export function schemaNodes(path: string, depth: number, db: DatabaseRef, schema: SchemaInfo, version: number, expanded: Expanded): TreeNode[] {
  const b = build(expanded);
  schemaRows(b, path, depth, db, schema, version);
  return b.out;
}

function ConnectionMenu({ config, shown }: { config: ConnectionConfig; shown: DatabaseRef[] | null }) {
  const connected = shown !== null;
  const open = useWorkspace((s) => s.open);
  return (
    <>
      {connected ? (
        <ContextMenuItem onSelect={() => void disconnect(config.id)}>Disconnect</ContextMenuItem>
      ) : (
        <ContextMenuItem onSelect={() => void connect(config.id)}>Connect</ContextMenuItem>
      )}
      <ContextMenuItem onSelect={() => openSqlTab({ db: shown?.[0] ?? { connectionId: config.id, database: config.database } })}>New SQL tab</ContextMenuItem>
      {connected && <ContextMenuItem onSelect={() => void refreshConnection(config.id)}>Refresh catalog</ContextMenuItem>}
      <ContextMenuSeparator />
      <ContextMenuItem testId="conn-edit" onSelect={() => open({ kind: "connection", editing: config.id })}>
        Edit…
      </ContextMenuItem>
      <ContextMenuItem danger onSelect={() => void confirmDelete(config.id)}>
        Delete
      </ContextMenuItem>
    </>
  );
}

/** Switching notebooks leaves their sessions alive, so a database row says when its notebook is still at work. */
function NotebookMarker({ db, testId }: { db: DatabaseRef; testId: string }) {
  const cells = useWorkspace(useShallow((s) => Object.values(s.notebooks).flatMap((n) => (sameDb(n.db, db) ? n.cells : []))));
  const activity = useLive((s) => notebookActivity(s.consoles, cells));
  if (activity === null) return null;
  return (
    <span data-testid={testId} className={cx("flex shrink-0 items-center gap-1 pl-2 text-ui-10.5", activity === "running" ? "text-run" : "text-amber")}>
      {activity === "running" ? <Spinner className="size-2.5 border-run" /> : <span className="size-1.5 rounded-full bg-amber" />}
      {activity === "running" ? "Running" : "Open transaction"}
    </span>
  );
}

/** A catalog that is loading or failed, as the live regions outside the tree say it. */
type CatalogNews = { loading: string[]; failed: string[] };

/** One shown database of a connected instance: its row, then its schemas once the catalog is in. */
function databaseRows(b: Build, news: CatalogNews, path: string, db: DatabaseRef, state: DatabaseState, views: Record<DbKey, CatalogView>) {
  const me = `${path}/${db.database}`;
  const open = b.expanded.has(me);
  const view = viewFor(views, db);
  const count = schemaCount(state, view?.catalog);
  const target: PickerTarget = { kind: "schemas", db };
  const picked = b.browse !== null && sameDb(b.browse.selected, db);
  row(b, {
    key: me,
    depth: 1,
    name: db.database,
    expandable: b.browse === null,
    selected: picked,
    open,
    picker: target,
    // Browsing, a database sits under its connection's icon, tied to it by a guide line, rather than a full level in.
    ...(b.browse && { height: 28, indent: 26, guide: true }),
    onClick: () => (b.browse ? b.browse.select(db) : b.expanded.toggle(me)),
    menu: () => <NewMenu db={db} />,
    view: (active) => ({
      icon: <DatabaseIcon size={14} className={picked ? "text-keyword" : "text-dim"} />,
      label: picked ? <span className="font-medium text-keyword">{db.database}</span> : db.database,
      trailing: (
        <>
          {b.browse ? (
            <Swap
              rest={<NotebookMarker db={db} testId={`notebook-marker-${me}`} />}
              control={count && <PickerPill target={target} count={count} active={active} testId={`schema-pill-${me}`} label={schemasText(count)} />}
              held={active}
            />
          ) : (
            count && <PickerPill target={target} count={count} active={active} testId={`schema-pill-${me}`} />
          )}
        </>
      ),
    }),
  });
  if (b.browse) {
    if (state.catalog.kind === "Idle" && sameDb(b.browse.selected, db)) b.idle.push(db);
    return;
  }
  if (!open) return;

  // Past the engine's cap of auto-opened databases a shown one stays Idle until its row opens. The snapshot then
  // moves it to Loading and Ready, and the catalog sync fetches it; the reply itself carries nothing new.
  if (state.catalog.kind === "Idle") b.idle.push(db);
  const catalog = state.catalog;
  if (catalog.kind === "Failed") {
    news.failed.push(`${db.database}: ${view ? "Refresh failed: " : ""}${catalog.message}`);
    note(b, `${me}/!error`, 1, 20, () => (
      <div data-testid={`db-error-${me}`} className="mb-1 flex items-baseline gap-2 pr-2 pl-[52px] text-ui-11.5 leading-4 text-danger/90">
        <span className="min-w-0 flex-1 truncate" title={catalog.message}>
          {view ? "Refresh failed: " : ""}
          {catalog.message}
        </span>
        <button
          type="button"
          data-testid={`db-retry-${me}`}
          onClick={() => void refreshCatalog(db)}
          className="shrink-0 rounded px-1 text-fg/80 underline-offset-2 hover:bg-control hover:underline"
        >
          Retry
        </button>
      </div>
    ));
  }
  if (!view) {
    if (catalog.kind !== "Failed") {
      news.loading.push(db.database);
      note(b, `${me}/!loading`, 1, 28, () => (
        <div className="flex h-7 items-center gap-2 pl-[44px] text-ui-12 text-dim">
          <Spinner /> Loading catalog
        </div>
      ));
    }
    return;
  }
  const schemas = state.shownSchemas.flatMap((name) => view.catalog.schemas.find((s) => s.name === name) ?? []);
  if (schemas.length === 0 && view.catalog.extensions.length === 0) {
    note(b, `${me}/!empty`, 1, 28, () => (
      <div data-testid={`db-empty-${me}`} className="flex h-7 items-center pl-[44px] text-ui-12 text-muted">
        No schemas shown
      </div>
    ));
  }
  for (const s of schemas) schemaRows(b, me, 2, db, s, view.catalog.version);
  objectGroup(
    b,
    `${me}/#extensions`,
    2,
    db,
    "Extensions",
    ZapIcon,
    view.catalog.extensions.map((ext) => ({ oid: ext.oid, name: ext.name, detail: ext.version, target: { kind: "Extension", oid: ext.oid } })),
  );
}

/**
 * A row's status at rest and the control that takes its place while the row is hovered or keyboard focused, or `held`.
 * Both share one cell, and the control only fades out, so it stays focusable and Tab can reach it from the row.
 */
function Swap({ rest, control, held }: { rest: ReactNode; control: ReactNode; held: boolean }) {
  return (
    <span className="grid shrink-0 items-center justify-items-end">
      <span
        className={cx(
          "col-start-1 row-start-1 flex items-center",
          held ? "invisible" : "group-hover:invisible group-focus-visible:invisible group-has-[:focus-visible]:invisible",
        )}
      >
        {rest}
      </span>
      <span
        className={cx(
          "col-start-1 row-start-1 flex items-center",
          !held && "opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100 group-has-[:focus-visible]:opacity-100",
        )}
      >
        {control}
      </span>
    </span>
  );
}

const schemasText = (c: ShownCount) => (c.shown === c.total ? `${c.total} ${c.total === 1 ? "schema" : "schemas"}` : `${c.shown} of ${c.total}`);

/**
 * A small pointer shortcut inside a row. It acts without selecting or toggling the row. It stays out of the Tab order,
 * since Enter and the context-menu key already do what it does, and every row would otherwise add stops.
 */
function RowButton({ label, onClick, children }: { label: string; onClick: (e: MouseEvent<HTMLButtonElement>) => void; children: ReactNode }) {
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-label={label}
      title={label}
      onClick={(e) => {
        e.stopPropagation();
        onClick(e);
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      className="grid size-5 shrink-0 place-items-center rounded text-muted hover:bg-control hover:text-fg"
    >
      {children}
    </button>
  );
}

/** Opens the row's own context menu below the button, so the menu has one definition however it opens. */
function RowMenuButton({ label }: { label: string }) {
  return (
    <RowButton
      label={label}
      onClick={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        e.currentTarget.dispatchEvent(new globalThis.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.left, clientY: r.bottom + 2 }));
      }}
    >
      <EllipsisIcon size={14} />
    </RowButton>
  );
}

const STATUS: Record<Tone, { label: string; dot: string | null }> = {
  live: { label: "Connected", dot: "bg-green" },
  pending: { label: "Connecting", dot: "bg-amber" },
  error: { label: "Connection failed", dot: "bg-danger" },
  off: { label: "Disconnected", dot: null },
};

/** A connection's icon, with a dot for its status: green connected, amber connecting, red failed, none when off. */
function ConnectionIcon({ tone, testId }: { tone: Tone; testId: string }) {
  const { label, dot } = STATUS[tone];
  return (
    <span data-testid={testId} role="img" aria-label={label} title={label} className="relative grid size-4 place-items-center">
      <DatabaseIcon size={14} aria-hidden className={tone === "off" ? "text-dim" : "text-keyword"} />
      {dot && <span aria-hidden className={cx("absolute -right-px -bottom-px size-[7px] rounded-full ring-[1.5px] ring-sidebar", dot)} />}
    </span>
  );
}

function ConnectButton({ config }: { config: ConnectionConfig }) {
  return (
    <button
      type="button"
      data-testid={`conn-connect-${config.name}`}
      onClick={(e) => {
        e.stopPropagation();
        void connect(config.id);
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      className="ml-2 h-5 shrink-0 rounded-[5px] border border-control bg-bg px-2 text-ui-11 font-medium text-fg hover:bg-hover"
    >
      Connect
    </button>
  );
}

/**
 * While browsing, a connection shows its environment badge, and its database picker and menu take that place on hover
 * or focus. An offline connection offers Connect the same way.
 */
function ConnectionTrailing({
  config,
  tone,
  instance,
  target,
  active,
  browse,
}: {
  config: ConnectionConfig;
  tone: Tone;
  instance: InstanceState | null;
  target: PickerTarget;
  active: boolean;
  browse: boolean;
}) {
  const badge = <EnvironmentBadge environment={config.environment} />;
  if (instance) {
    const count = databaseCount(instance);
    if (!browse) return <PickerPill target={target} count={count} active={active} testId={`db-pill-${config.name}`} />;
    const controls = (
      <span className="flex items-center gap-0.5">
        <PickerPill target={target} count={count} active={active} testId={`db-pill-${config.name}`} label={`${count.shown} of ${count.total}`} />
        <RowMenuButton label={`${config.name} options`} />
      </span>
    );
    return <Swap rest={badge} control={controls} held={active} />;
  }
  switch (tone) {
    case "pending":
      return (
        <span className="grid shrink-0 place-items-center pl-2">
          <Spinner />
        </span>
      );
    case "error":
      return browse ? (
        <span className="shrink-0 pl-2 text-ui-11 font-medium text-danger">Failed</span>
      ) : (
        <span className="flex shrink-0 items-center gap-1.5 pl-2 text-ui-11 font-normal text-muted">
          <StatusDot tone="error" />
          Failed
        </span>
      );
    case "live":
      return browse ? badge : null;
    case "off":
      return browse ? <Swap rest={badge} control={<ConnectButton config={config} />} held={false} /> : <ConnectButton config={config} />;
    default: {
      const unreachable: never = tone;
      return unreachable;
    }
  }
}

/** A failed connection explains itself under its row, with the two ways forward. */
function ConnectionError({ config, failure }: { config: ConnectionConfig; failure: string }) {
  return (
    <div className="pb-1 pl-6">
      <div data-testid={`conn-error-${config.name}`} className="rounded-md bg-danger/8 px-2.5 py-2">
        <p title={failure} className="line-clamp-2 text-ui-11 leading-4 text-danger">
          {failure}
        </p>
        <div className="mt-1.5 flex gap-3 text-ui-11 font-medium text-fg">
          <button type="button" className="hover:underline" onClick={() => void connect(config.id)}>
            Retry
          </button>
          <button type="button" className="hover:underline" onClick={() => useWorkspace.getState().open({ kind: "connection", editing: config.id })}>
            Edit connection…
          </button>
        </div>
      </div>
    </div>
  );
}

type Revealed = Map<ConnectionId, "database" | "schema">;

/** The shading a connection's rows share, or null while tinting is off or the connection has no color. */
function tintOf(config: ConnectionConfig, tinting: boolean): TintGroup | null {
  const hex = tinting ? connectionColor(config.color) : null;
  return hex ? { name: config.name, hex } : null;
}

function connectionRows(b: Build, news: CatalogNews, config: ConnectionConfig, state: ConnectionState | undefined, views: Record<DbKey, CatalogView>) {
  const instance = instanceOf(state);
  const tone = connectionTone(state);
  const path = config.name;
  const open = b.expanded.has(path);
  const connected = instance !== null;
  const shown = instance?.databases.filter((d) => d.shown) ?? [];
  const target: PickerTarget = { kind: "databases", id: config.id };
  const failure =
    state?.status.kind === "Failed" ? connectErrorText(state.status.error) : state?.status.kind === "Lost" ? `Connection lost: ${state.status.reason}` : null;
  const tinted = b.tint !== null;
  row(b, {
    key: path,
    depth: 0,
    name: config.name,
    expandable: true,
    open: open && connected,
    strong: true,
    ...(b.browse && { height: 30, indent: 6, gapBefore: b.out.length > 0 }),
    picker: target,
    onClick: () => {
      if (connected || state?.status.kind === "Connecting") b.expanded.toggle(path);
      else void connect(config.id);
    },
    menu: () => <ConnectionMenu config={config} shown={instance && shown.map((d) => ({ connectionId: config.id, database: d.name }))} />,
    view: (active) => ({
      icon: b.browse ? (
        <ConnectionIcon tone={tone} testId={`conn-status-${config.name}`} />
      ) : (
        <EnvironmentIcon environment={config.environment} size={15} testId={`env-icon-${config.name}`} />
      ),
      label: (
        <span className="flex min-w-0 items-center">
          <span className={cx("truncate", tone === "off" && "text-muted")}>{config.name}</span>
          {!tinted && <ConnectionColorDot color={config.color} className="ml-1.5" />}
        </span>
      ),
      trailing: <ConnectionTrailing config={config} tone={tone} instance={instance} target={target} active={active} browse={b.browse !== null} />,
    }),
  });
  if (failure && b.browse) note(b, `${path}/!error`, 0, 64, () => <ConnectionError config={config} failure={failure} />);
  else if (failure) {
    note(b, `${path}/!error`, 0, 20, () => (
      <div data-testid={`conn-error-${config.name}`} title={failure} className="mb-1 line-clamp-2 pr-2 pl-[36px] text-ui-11 leading-4 text-danger/90">
        {failure}
      </div>
    ));
  }
  if (!open || !connected) return;
  for (const d of shown) databaseRows(b, news, path, { connectionId: config.id, database: d.name }, d, views);
}

/** A container row while filtering: it opens and closes and matches nothing itself. */
function foldRow(b: Build, path: string, depth: number, name: string, icon: ReactNode, label: ReactNode = name, strong = false) {
  row(b, {
    key: path,
    depth,
    name,
    expandable: true,
    open: b.expanded.has(path),
    strong,
    onClick: () => b.expanded.toggle(path),
    view: () => ({ icon, label }),
  });
}

function filteredRelationRows(b: Build, path: string, db: DatabaseRef, schema: string, { rel, ranges, columns }: FilteredRelation) {
  const me = `${path}/${rel.name}`;
  const expandable = columns.length > 0;
  const open = expandable && b.expanded.has(me);
  row(b, {
    key: me,
    depth: 3,
    name: rel.name,
    expandable,
    keepChevronSpace: true,
    quietChevron: true,
    open,
    selected: isSelected(b.selected, db, schema, rel),
    ...relationActions(db, schema, rel),
    onToggle: expandable ? () => b.expanded.toggle(me) : undefined,
    view: () => {
      const selected = isSelected(b.selected, db, schema, rel);
      return {
        icon: relationIcon(rel, selected),
        label: (
          <span className={cx("block min-w-0", selected && "font-medium")}>
            <RelationName name={rel.name} ranges={ranges} />
          </span>
        ),
        trailing: <RelationTrailing db={db} schema={schema} rel={rel} />,
      };
    },
  });
  if (open) for (const c of columns) columnRow(b, `${me}/#columns`, 4, c.column, c.ranges);
}

function filteredSchemaRows(b: Build, path: string, db: DatabaseRef, match: FilteredSchema) {
  const me = `${path}/${match.name}`;
  const functions = `${me}/#functions`;
  const total = b.total?.(db, match.name) ?? null;
  const kept = match.relations.length;
  row(b, {
    key: me,
    depth: 2,
    name: match.name,
    expandable: true,
    open: b.expanded.has(me),
    strong: true,
    height: 28,
    onClick: () => b.expanded.toggle(me),
    view: () => ({ label: <SchemaName name={match.name} />, trailing: kept > 0 && <Count>{total === null ? kept : `${kept} of ${total}`}</Count> }),
  });
  if (!b.expanded.has(me)) return;
  for (const r of match.relations) filteredRelationRows(b, me, db, match.name, r);
  if (match.functions.length > 0) foldRow(b, functions, 3, "Functions", <BracesIcon size={14} className="text-muted" />);
  if (b.expanded.has(functions)) for (const f of match.functions) functionRow(b, functions, 4, db, f.fn, f.ranges);
  objectGroup(
    b,
    `${me}/#sequences`,
    3,
    db,
    "Sequences",
    HashIcon,
    match.sequences.map(({ seq, ranges }) => ({ oid: seq.oid, name: seq.name, detail: seq.typeName, ranges, target: { kind: "Sequence", oid: seq.oid } })),
  );
  objectGroup(
    b,
    `${me}/#types`,
    3,
    db,
    "Types",
    TypeIcon,
    match.types.map(({ type, ranges }) => ({ oid: type.oid, name: type.name, detail: typeDetail(type.kind), ranges, target: { kind: "Type", oid: type.oid } })),
  );
  objectGroup(
    b,
    `${me}/#synonyms`,
    3,
    db,
    "Synonyms",
    LinkIcon,
    match.synonyms.map(({ syn, ranges }) => ({ oid: syn.oid, name: syn.name, detail: syn.target, ranges, target: { kind: "Synonym", oid: syn.oid } })),
  );
}

/** One schema's rows in the tree narrowed to a search. */
export function filteredSchemaNodes(path: string, db: DatabaseRef, match: FilteredSchema, expanded: Expanded): TreeNode[] {
  const b = build(expanded);
  filteredSchemaRows(b, path, db, match);
  return b.out;
}

/** The tree narrowed to a search, every node open until its chevron closes it. */
function filteredTree(b: Build, filter: TreeFilter, tinting: boolean) {
  for (const { config, databases } of filter.connections) {
    b.tint = tintOf(config, tinting);
    foldRow(
      b,
      config.name,
      0,
      config.name,
      <EnvironmentIcon environment={config.environment} size={15} testId={`env-icon-${config.name}`} />,
      <span className="font-medium">{config.name}</span>,
    );
    if (!b.expanded.has(config.name)) continue;
    for (const { db, schemas, extensions } of databases) {
      const path = `${config.name}/${db.database}`;
      foldRow(b, path, 1, db.database, <DatabaseIcon size={15} className="text-muted" />);
      if (!b.expanded.has(path)) continue;
      for (const s of schemas) filteredSchemaRows(b, path, db, s);
      objectGroup(
        b,
        `${path}/#extensions`,
        2,
        db,
        "Extensions",
        ZapIcon,
        extensions.map(({ ext, ranges }) => ({ oid: ext.oid, name: ext.name, detail: ext.version, ranges, target: { kind: "Extension", oid: ext.oid } })),
      );
    }
  }
}

/** How many of a schema's relations the category shows, or everything for All. */
function inCategory(schema: SchemaInfo | undefined, category: "tables" | "views" | "all"): number | null {
  if (!schema) return null;
  if (category === "all") return schema.relations.length + schema.functions.length + schema.sequences.length + schema.types.length + schema.synonyms.length;
  return schema.relations.filter((r) => isViewKind(r.kind) === (category === "views")).length;
}

/** The object list hangs off its schema headings: a table's icon lines up with its schema's name, and deeper rows step in from there. */
const objectIndent = (nodes: TreeNode[]): TreeNode[] => nodes.map((n) => (n.kind === "row" ? { ...n, indent: 6 + Math.max(0, n.depth - 1) * INDENT } : n));

const SKELETON = [148, 112, 176, 96, 132, 160, 104];

function ObjectsLoading() {
  return (
    <div role="status" className="px-3">
      <p className="flex h-[34px] items-center text-ui-11.5 text-dim">Loading database objects…</p>
      {SKELETON.map((width, i) => (
        <div key={i} aria-hidden className="flex h-[26px] items-center gap-2 pl-3">
          <span className="size-3.5 rounded-[3px] bg-control/60" />
          <span className="h-2 rounded-full bg-control/60" style={{ width }} />
        </div>
      ))}
    </div>
  );
}

function TreeNote({ children }: { children: ReactNode }) {
  return (
    <p data-testid="tree-search-empty" className="px-4 pt-3.5 pb-1.5 text-ui-12.5 text-muted">
      {children}
    </p>
  );
}

function useOpenSet(): [ReadonlySet<string>, (path: string, force?: boolean) => void] {
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const toggle = useCallback(
    (p: string, force?: boolean) =>
      setOpen((prev) => {
        const next = new Set(prev);
        if (force ?? !prev.has(p)) next.add(p);
        else next.delete(p);
        return next;
      }),
    [],
  );
  return [open, toggle];
}

/** The search's closed rows, which start over with each new query: every searched node starts open. */
function useCollapsedSet(query: string): Expanded {
  const [state, setState] = useState<{ query: string; closed: ReadonlySet<string> }>({ query, closed: new Set() });
  const closed = state.query === query ? state.closed : null;
  const toggle = useCallback(
    (p: string, open?: boolean) =>
      setState((prev) => {
        const next = new Set(prev.query === query ? prev.closed : []);
        if (open ?? next.has(p)) next.delete(p);
        else next.add(p);
        return { query, closed: next };
      }),
    [query],
  );
  return useMemo(() => ({ has: (p) => !closed?.has(p), toggle }), [closed, toggle]);
}

const NO_NEWS: CatalogNews = { loading: [], failed: [] };

/** `filter` is set while the sidebar search holds a query, and the tree then shows only its matches. */
type TreeScope =
  | { kind: "databases"; selected: DatabaseRef | null; select(db: DatabaseRef): void }
  | { kind: "objects"; db: DatabaseRef; category: "tables" | "views" | "all" };
export function ConnectionTree({
  filter,
  query,
  ref,
  scope,
}: {
  filter: TreeFilter | null;
  query: string;
  ref: RefObject<HTMLDivElement | null>;
  scope?: TreeScope;
}) {
  const configs = useWorkspace((s) => s.configs);
  const selected = useWorkspace((s) => s.selected);
  const tinting = useWorkspace((s) => s.settings.tintConnections);
  const connections = useLive((s) => s.connections);
  const views = useCatalog((s) => s.views);
  const entries = useRelationDetail((s) => s.entries);
  // Held here, not in the rows, so a search that swaps the rows out does not reopen what the user closed.
  const [open, toggle] = useOpenSet();
  const expanded = useMemo<Expanded>(() => ({ has: (p) => open.has(p), toggle }), [open, toggle]);
  const [revealed] = useState<Revealed>(() => new Map());
  const searched = useCollapsedSet(query);
  const list = useMemo(() => Object.values(configs).sort((a, b) => a.name.localeCompare(b.name)), [configs]);

  const tree = useMemo(() => {
    if (filter && scope?.kind === "databases") {
      const b = build(searched, selected);
      b.browse = scope;
      const tokens = parseQuery(query);
      for (const config of list) {
        const all = matchRanges(tokens, config.name) !== null;
        const databases = instanceOf(connections[config.id])?.databases.filter((d) => d.shown && matchRanges(tokens, d.name) !== null) ?? [];
        if (!all && databases.length === 0) continue;
        const start = b.out.length;
        connectionRows(b, { loading: [], failed: [] }, config, connections[config.id], views);
        if (!all) {
          const allowed = new Set(databases.map((d) => `${config.name}/${d.name}`));
          b.out = [...b.out.slice(0, start), ...b.out.slice(start).filter((n) => n.depth === 0 || allowed.has(n.key))];
        }
      }
      return { b, news: NO_NEWS };
    }
    if (filter) {
      const b = build(searched, selected);
      if (scope?.kind === "objects")
        b.total = (db, name) =>
          inCategory(
            viewFor(views, db)?.catalog.schemas.find((s) => s.name === name),
            scope.category,
          );
      filteredTree(b, filter, tinting);
      if (scope?.kind === "objects") {
        const prefix = `${configs[scope.db.connectionId]?.name}/${scope.db.database}/`;
        b.out = objectIndent(b.out.filter((n) => n.key.startsWith(prefix)).map((n) => ({ ...n, depth: Math.max(0, n.depth - 2) })));
        if (b.out.length > 0)
          note(b, "objects-hint", 0, 36, () => <p className="px-3 pt-2.5 text-ui-11 text-dim">Enter opens the first match. Esc clears the filter.</p>);
      }
      return { b, news: NO_NEWS };
    }
    const b = build(expanded, selected, entries);
    const news: CatalogNews = { loading: [], failed: [] };
    if (scope?.kind === "objects") {
      const view = viewFor(views, scope.db);
      const state = instanceOf(connections[scope.db.connectionId])?.databases.find((d) => d.name === scope.db.database);
      if (!view) {
        const failed = state?.catalog.kind === "Failed" ? state.catalog.message : null;
        const connected = instanceOf(connections[scope.db.connectionId]) !== null;
        if (failed || !connected)
          note(b, "objects-status", 0, 40, () => <p className="px-3 py-3 text-ui-12 text-dim">{failed ?? "Connect to load database objects."}</p>);
        else note(b, "objects-status", 0, 34 + SKELETON.length * 26, () => <ObjectsLoading />);
      }
      if (view)
        for (const schema of view.catalog.schemas.filter((s) => !state || state.shownSchemas.includes(s.name))) {
          const shown =
            scope.category === "all"
              ? schema
              : {
                  ...schema,
                  tableCount: schema.relations.filter((r) => isViewKind(r.kind) === (scope.category === "views")).length,
                  relations: schema.relations.filter((r) => isViewKind(r.kind) === (scope.category === "views")),
                  functions: [],
                  sequences: [],
                  synonyms: [],
                  types: [],
                };
          schemaRows(b, `${configs[scope.db.connectionId]?.name}/${scope.db.database}`, 0, scope.db, shown, view.catalog.version);
        }
      if (view && scope.category === "all")
        objectGroup(
          b,
          `${configs[scope.db.connectionId]?.name}/${scope.db.database}/#extensions`,
          0,
          scope.db,
          "Extensions",
          ZapIcon,
          view.catalog.extensions.map((ext) => ({ oid: ext.oid, name: ext.name, detail: ext.version, target: { kind: "Extension", oid: ext.oid } })),
        );
      b.out = objectIndent(b.out);
      return { b, news };
    }
    if (scope?.kind === "databases") b.browse = scope;
    for (const config of list) {
      b.tint = tintOf(config, tinting);
      connectionRows(b, news, config, connections[config.id], views);
    }
    return { b, news };
  }, [filter, searched, expanded, selected, entries, tinting, list, connections, views, scope, configs, query]);

  const objectsDb = scope?.kind === "objects" ? scope.db : null;
  const objectDefault = viewFor(views, objectsDb)?.catalog.defaultSchema;
  const objectConnection = objectsDb ? configs[objectsDb.connectionId]?.name : null;
  useEffect(() => {
    if (objectsDb && objectDefault) toggle(`${objectConnection}/${objectsDb.database}/${objectDefault}`, true);
  }, [objectsDb, objectDefault, objectConnection, toggle]);

  // The first connect opens the connection and its home database, then that database's default schema once its catalog says which.
  useEffect(() => {
    if (filter) return;
    for (const config of list) {
      const home = instanceOf(connections[config.id])?.home;
      if (!home) continue;
      const path = config.name;
      if (!revealed.has(config.id)) {
        revealed.set(config.id, "database");
        toggle(path, true);
        toggle(`${path}/${home}`, true);
      }
      const homeDefault = viewFor(views, { connectionId: config.id, database: home })?.catalog.defaultSchema;
      if (revealed.get(config.id) === "database" && homeDefault !== undefined) {
        revealed.set(config.id, "schema");
        if (homeDefault) toggle(`${path}/${home}/${homeDefault}`, true);
      }
    }
  }, [filter, list, connections, views, toggle, revealed]);

  // Each fetch starts when its row opens, or when the catalog version moves under an open relation, and not again until then.
  const requested = useRef(new Set<string>());
  const ensured = useRef(new Map<string, number>());
  useEffect(() => {
    const idle = new Set<string>();
    for (const db of tree.b.idle) {
      const key = dbKey(db);
      idle.add(key);
      if (!requested.current.has(key)) void commands.catalogGet(db);
    }
    requested.current = idle;
    const details = new Map<string, number>();
    for (const { db, version, oid } of tree.b.details) {
      const key = detailKey(db, oid);
      details.set(key, version);
      if (ensured.current.get(key) !== version) void useRelationDetail.getState().ensure(db, version, oid);
    }
    ensured.current = details;
  }, [tree]);

  return (
    <>
      <div
        ref={ref}
        data-testid={scope?.kind === "objects" ? "object-tree" : "conn-tree"}
        role="tree"
        aria-label={scope?.kind === "objects" ? "Database objects" : "Connections"}
        className="min-h-0 flex-1 overflow-y-auto"
      >
        {filter && filter.searched === 0 && scope?.kind !== "databases" ? (
          <TreeNote>Connect to a database to search its catalog.</TreeNote>
        ) : filter && tree.b.out.length === 0 ? (
          <TreeNote>No matches for “{query.trim()}”.</TreeNote>
        ) : (
          <VirtualTree key={filter ? `search:${query}` : "tree"} nodes={tree.b.out} scrollRef={ref} />
        )}
      </div>
      {/* The loading and failure notes are rows that scroll in and out of the DOM, so the announcements live here, outside the tree. */}
      <div role="status" className="sr-only">
        {tree.news.loading.length > 0 ? `Loading catalog: ${tree.news.loading.join(", ")}` : ""}
      </div>
      <div role="alert" className="sr-only">
        {tree.news.failed.length > 0 ? `Catalog failed for ${tree.news.failed.join("; ")}` : ""}
      </div>
    </>
  );
}
