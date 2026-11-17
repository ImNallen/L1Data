import { ArrowLeftIcon, ArrowRightIcon, PlayIcon, PlusIcon, Table2Icon, XIcon } from "lucide-react";
import { useEffect, type ReactNode } from "react";
import type { MapColumnPair, RelationDetail, RelKind } from "../../ipc/bindings";
import { detailKey, useRelationDetail } from "../../state/relationDetail";
import { Button, cx, IconButton, Spinner } from "../../ui/primitives";
import { defaultText, indexText, triggerText, type RowText } from "../sidebar/detailText";
import { browseRows, queryInNewCell } from "./MapCell";
import type { ErColumn, ErLink, ErModel, ErTable, Oid } from "./model";
import { compactCount, ROLE_ICON, shortType } from "./SchemaCanvas";
import type { SchemaMapView } from "./useSchemaMap";

const KIND_LABEL: Record<RelKind, string> = {
  Table: "Table",
  PartitionedTable: "Partitioned table",
  View: "View",
  MaterializedView: "Materialized view",
  ForeignTable: "Foreign table",
};

function Section({ title, count, note, inferred, children }: { title: string; count?: number; note?: string; inferred?: boolean; children: ReactNode }) {
  return (
    <section aria-label={title} className="flex flex-col gap-1.5 border-t border-border px-4 py-3">
      <h3 className={cx("text-ui-12 font-semibold", inferred ? "text-amber" : "text-fg")}>
        {title}
        {count !== undefined && <span className="ml-1.5 font-normal text-dim tabular-nums">{count}</span>}
      </h3>
      {note && <p className="-mt-1 text-ui-11 text-dim">{note}</p>}
      {children}
    </section>
  );
}

const None = () => <p className="text-ui-11.5 text-dim">None</p>;

const names = (pairs: readonly MapColumnPair[], end: "source" | "target") => (pairs.length === 1 ? pairs[0]![end] : `(${pairs.map((p) => p[end]).join(", ")})`);

const pairText = (pairs: readonly MapColumnPair[]) => `${names(pairs, "source")} → ${names(pairs, "target")}`;

function LinkRow({ link, from, model, onFocus }: { link: ErLink; from: Oid; model: ErModel; onFocus: (table: ErTable) => void }) {
  const outgoing = link.source === from;
  const other = model.tables.get(outgoing ? link.target : link.source)!;
  const inferred = link.basis === "Inferred";
  const Arrow = outgoing ? ArrowRightIcon : ArrowLeftIcon;
  const name = other.external ? `${other.schema}.${other.name}` : other.name;
  return (
    <button
      type="button"
      data-link={link.id}
      title={`Focus ${name}`}
      onClick={() => onFocus(other)}
      className={cx(
        "flex w-full flex-col gap-0.5 rounded-md border px-2.5 py-1.5 text-left hover:bg-hover",
        inferred ? "border-dashed border-amber/50" : "border-control",
      )}
    >
      <span className="flex w-full items-center gap-2">
        <Arrow size={12} aria-hidden className={cx("shrink-0", inferred ? "text-amber" : "text-keyword")} />
        <span className="min-w-0 flex-1 truncate font-mono text-ui-12 font-semibold text-fg">{name}</span>
        <span className="shrink-0 text-ui-11 text-dim tabular-nums">
          {outgoing ? `${link.pairs.length} ${link.pairs.length === 1 ? "col" : "cols"}` : other.rowEstimate !== null ? compactCount(other.rowEstimate) : null}
        </span>
      </span>
      <span className="pl-5 font-mono text-ui-11 [overflow-wrap:anywhere] text-muted">{pairText(link.pairs)}</span>
    </button>
  );
}

function declaredNarrowestFirst(a: ErLink, b: ErLink): number {
  return Number(a.basis === "Inferred") - Number(b.basis === "Inferred") || a.pairs.length - b.pairs.length;
}

function columnNote(column: ErColumn, outgoing: readonly ErLink[], model: ErModel, detail: RelationDetail | null): { text: string; inferred?: boolean } | null {
  const link = outgoing.find((l) => l.pairs.some((p) => p.source === column.name));
  if (link) return { text: `→ ${model.tables.get(link.target)!.name}`, inferred: link.basis === "Inferred" };
  const value = detail?.defaults.find((d) => d.column === column.name)?.value;
  if (value?.kind === "Identity") return { text: defaultText(value) };
  if (detail?.keys.some((k) => k.kind.kind === "Unique" && k.columns.length === 1 && k.columns[0] === column.name)) return { text: "unique" };
  if (value) return { text: value.kind === "Expr" ? `default ${value.sql}` : defaultText(value) };
  return null;
}

function DetailRows({ rows }: { rows: readonly ({ name: string } & RowText)[] }) {
  if (!rows.length) return <None />;
  return (
    <ul className="flex flex-col gap-1.5">
      {rows.map((row) => (
        <li key={row.name} className="flex flex-col gap-0.5">
          <span className="flex items-baseline gap-2">
            <span className="min-w-0 flex-1 truncate font-mono text-ui-11.5 text-fg">{row.name}</span>
            <span className="shrink-0 text-ui-11 text-dim">{row.trailing}</span>
          </span>
          <span className="truncate font-mono text-ui-11 text-muted" title={row.second}>
            {row.second}
          </span>
        </li>
      ))}
    </ul>
  );
}

export function MapInspector({ map, closeThen }: { map: SchemaMapView; closeThen: (go: () => Promise<void>) => void }) {
  const { table, links } = map.focus!;
  const model = map.model!;
  const db = map.db;
  const entry = useRelationDetail((s) => (db ? s.entries[detailKey(db, table.oid)] : undefined));
  useEffect(() => {
    if (db) void useRelationDetail.getState().ensure(db, model.version, table.oid);
  }, [db, model.version, table.oid]);

  const detail = entry?.detail ?? null;
  const declared = links.filter((l) => l.basis === "Declared");
  const references = declared.filter((l) => l.source === table.oid);
  const referencedBy = declared.filter((l) => l.target === table.oid);
  const possible = links.filter((l) => l.basis === "Inferred");
  const outgoing = links.filter((l) => l.source === table.oid).sort(declaredNarrowestFirst);
  const focus = (other: ErTable) => map.actions.focus(other.key);
  const detailBody = (rows: (d: RelationDetail) => readonly ({ name: string } & RowText)[]) =>
    detail ? (
      <DetailRows rows={rows(detail)} />
    ) : entry?.error ? (
      <p className="text-ui-11.5 text-danger">{entry.error}</p>
    ) : (
      <p className="flex items-center gap-2 text-ui-11.5 text-dim">
        <Spinner /> Loading…
      </p>
    );

  return (
    <aside data-testid="map-inspector" aria-label={`${table.name} details`} className="flex w-[312px] shrink-0 flex-col overflow-y-auto border-l border-border">
      <div className="flex flex-col gap-1 px-4 pt-3 pb-3">
        <div className="flex items-center gap-2">
          <Table2Icon size={14} aria-hidden className="shrink-0 text-keyword" />
          <h2 className="min-w-0 flex-1 truncate font-mono text-ui-14 font-semibold text-fg">{table.name}</h2>
          <IconButton aria-label="Clear focus" title="Clear focus" className="-mr-1.5 size-6" onClick={() => map.actions.focus(null)}>
            <XIcon size={14} />
          </IconButton>
        </div>
        <p className="text-ui-11.5 text-dim">
          {table.schema} · {KIND_LABEL[table.kind]}
          {table.rowEstimate !== null && ` · ~${compactCount(table.rowEstimate)} rows`}
        </p>
        <div className="mt-2 flex gap-2">
          <Button variant="primary" className="h-7 flex-1 text-ui-12" disabled={!db} onClick={() => closeThen(() => browseRows(map, table))}>
            <PlayIcon size={12} aria-hidden />
            Browse rows
          </Button>
          <Button className="h-7 flex-1 text-ui-12" disabled={!db} onClick={() => closeThen(() => queryInNewCell(map, table))}>
            <PlusIcon size={13} aria-hidden />
            New query
          </Button>
        </div>
      </div>

      <Section title="Columns" count={table.columns.length}>
        <ul className="flex flex-col gap-1">
          {table.columns.map((column) => {
            const role = ROLE_ICON[column.role];
            const Icon = role?.icon;
            const note = columnNote(column, outgoing, model, detail);
            return (
              <li key={column.attnum} data-column={column.name} className="flex items-center gap-2 font-mono text-ui-11.5">
                <span className="grid w-3 shrink-0 place-items-center">{Icon && <Icon size={11} className={role.className} aria-label={role.label} />}</span>
                <span className={cx("w-[108px] shrink-0 truncate", column.role === "plain" ? "text-muted" : "text-fg")} title={column.name}>
                  {column.name}
                </span>
                <span className="max-w-24 shrink-0 truncate text-ui-11 text-dim">{shortType(column.type)}</span>
                <span className={cx("min-w-0 flex-1 truncate text-right text-ui-11", note?.inferred ? "text-amber" : "text-dim")} title={note?.text}>
                  {note?.text}
                </span>
              </li>
            );
          })}
        </ul>
      </Section>

      <Section title="References" count={references.length}>
        {references.length ? references.map((l) => <LinkRow key={l.id} link={l} from={table.oid} model={model} onFocus={focus} />) : <None />}
      </Section>
      <Section title="Referenced by" count={referencedBy.length}>
        {referencedBy.length ? referencedBy.map((l) => <LinkRow key={l.id} link={l} from={table.oid} model={model} onFocus={focus} />) : <None />}
      </Section>
      {possible.length > 0 && (
        <Section title="Possible references" count={possible.length} inferred note="Same column name, no declared foreign key.">
          {possible.map((l) => (
            <LinkRow key={l.id} link={l} from={table.oid} model={model} onFocus={focus} />
          ))}
        </Section>
      )}
      <Section title="Indexes" count={detail?.indexes.length}>
        {detailBody((d) => d.indexes.map((i) => ({ name: i.name, ...indexText(i) })))}
      </Section>
      <Section title="Triggers" count={detail?.triggers.length}>
        {detailBody((d) => d.triggers.map((t) => ({ name: t.name, ...triggerText(t) })))}
      </Section>
    </aside>
  );
}
