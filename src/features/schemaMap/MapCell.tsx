import * as Menu from "@radix-ui/react-dropdown-menu";
import {
  ChevronDownIcon,
  KeyRoundIcon,
  LayoutGridIcon,
  Link2Icon,
  PlayIcon,
  PlusIcon,
  RefreshCwIcon,
  SearchIcon,
  Table2Icon,
  TriangleAlertIcon,
} from "lucide-react";
import { useRef, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import type { MapColumns } from "../../ipc/bindings";
import { Button, cx, IconButton, Kbd, MenuContent, MenuItem, Spinner } from "../../ui/primitives";
import { useNow } from "../../ui/useNow";
import { timeAgo } from "../assistant/assistantText";
import { browseTemplate, openSqlTab, openTableTab } from "../editor/runActions";
import { plural } from "../editor/writeQuestions";
import { groupCount } from "../results/format";
import { compactCount, SchemaCanvas } from "./SchemaCanvas";
import type { ErModel, ErTable } from "./model";
import type { SchemaMapView } from "./useSchemaMap";

const COLUMN_MODES: Record<MapColumns, string> = { All: "All", Keys: "Keys" };
const TOOL = "inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md border border-control px-2.5 text-ui-12 text-muted hover:bg-hover";

function ToolbarMenu({ label, value, testId, children }: { label: string; value: string; testId: string; children: ReactNode }) {
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger data-testid={testId} className={cx(TOOL, "data-[state=open]:bg-hover")}>
        {label}
        <span className="font-medium text-fg">{value}</span>
        <ChevronDownIcon size={12} className="text-dim" />
      </Menu.Trigger>
      <MenuContent>{children}</MenuContent>
    </Menu.Root>
  );
}

export function MapToolbar({ map, findRef, className }: { map: SchemaMapView; findRef?: React.Ref<HTMLInputElement>; className?: string }) {
  const first = () => (map.model && map.matches ? [...map.model.tables.values()].find((t) => map.matches!.has(t.oid)) : undefined);
  return (
    <div className={cx("flex items-center gap-2 border-b border-border px-3 py-2", className)}>
      <label className="flex h-7 w-60 min-w-0 shrink items-center gap-2 rounded-md border border-control bg-bg px-2 text-ui-12 focus-within:border-focus">
        <SearchIcon size={13} className="shrink-0 text-dim" />
        <input
          ref={findRef}
          data-testid="map-find"
          aria-label="Find table or column"
          placeholder="Find table or column…"
          className="min-w-0 flex-1 bg-transparent text-fg outline-none placeholder:text-dim"
          value={map.view.find}
          onChange={(e) => map.actions.find(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && map.view.find) {
              e.preventDefault();
              e.stopPropagation();
              map.actions.find("");
            } else if (e.key === "Enter") {
              const table = first();
              if (table) map.actions.focus(table.key);
            }
          }}
        />
        <Kbd className="shrink-0 px-1">⌘F</Kbd>
      </label>
      <ToolbarMenu label="Schema" value={map.schema ?? "–"} testId="map-schema">
        {map.schemas.map((name) => (
          <MenuItem key={name} checked={name === map.schema} onSelect={() => map.actions.setSchema(name)}>
            <span className="font-mono text-ui-12">{name}</span>
          </MenuItem>
        ))}
      </ToolbarMenu>
      <ToolbarMenu label="Columns" value={COLUMN_MODES[map.columns]} testId="map-columns">
        {(Object.keys(COLUMN_MODES) as MapColumns[]).map((mode) => (
          <MenuItem key={mode} testId={`map-columns-${mode}`} checked={mode === map.columns} onSelect={() => map.actions.setColumns(mode)}>
            {mode === "All" ? "All columns" : "Keys only"}
          </MenuItem>
        ))}
      </ToolbarMenu>
      <span className="flex-1" />
      <button type="button" className={cx(TOOL, "hover:text-fg")} onClick={map.actions.autoLayout}>
        <LayoutGridIcon size={13} />
        Auto layout
      </button>
    </div>
  );
}

function CrowsFoot() {
  return (
    <svg width="26" height="10" viewBox="0 0 26 10" aria-hidden className="text-dim">
      <path d="M 1 1 L 9 5 L 1 9 M 1 5 L 25 5 M 12 1 L 12 9 M 21 1 L 21 9" fill="none" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}

// The engine's schema_map limits.
const TABLE_LIMIT = 500;
const KEY_LIMIT = 2_000;
const PARTITION_LIMIT = 10_000;

function partialMapText({ omittedTables, keysTruncated, partitionsTruncated }: ErModel): string | null {
  const left: [string, string][] = [];
  if (omittedTables > 0) left.push([`${groupCount(omittedTables)} ${omittedTables === 1 ? "table" : "tables"}`, `${groupCount(TABLE_LIMIT)} tables`]);
  if (keysTruncated) left.push(["some keys", `${groupCount(KEY_LIMIT)} keys`]);
  const parts = left.length ? [`${left.map(([what]) => what).join(" and ")} left out (limit ${left.map(([, limit]) => limit).join(", ")})`] : [];
  if (partitionsTruncated) parts.push(`partitions past ${groupCount(PARTITION_LIMIT)} drawn as tables`);
  return parts.length ? `Partial map: ${parts.join("; ")}` : null;
}

function PartialMapNote({ model, short = false }: { model: ErModel | null; short?: boolean }) {
  const text = model && partialMapText(model);
  if (!text) return null;
  return (
    <span data-testid="map-partial" title={text} className={cx("flex min-w-0 items-center gap-1 text-amber", short && "shrink-0")}>
      <TriangleAlertIcon size={12} aria-hidden className="shrink-0" />
      <span className="truncate">{short ? "Partial map" : text}</span>
    </span>
  );
}

export function MapLegend({ map }: { map: SchemaMapView }) {
  const now = useNow(true, 30_000);
  const model = map.model;
  const declared = model ? [...model.links.values()].filter((l) => l.basis === "Declared").length : 0;
  const fetched = map.status.kind === "ready" || map.status.kind === "refreshing" ? map.status.fetchedAtMs : null;
  return (
    <div data-testid="map-legend" className="flex min-h-9 flex-wrap items-center gap-x-3 gap-y-1 px-3 py-1.5 text-ui-11 whitespace-nowrap text-muted">
      <span className="flex items-center gap-1">
        <KeyRoundIcon size={12} className="text-amber" /> Primary key
      </span>
      <span className="flex items-center gap-1">
        <Link2Icon size={12} className="text-keyword" /> Foreign key
      </span>
      <span className="flex items-center gap-1">
        <CrowsFoot /> Declared (many → one)
      </span>
      <span className="flex items-center gap-1">
        <span aria-hidden className="w-6 border-t border-dashed border-amber" /> Inferred by name
      </span>
      <span className="flex-1" />
      <PartialMapNote model={model} />
      {model && (
        <span data-testid="map-size" className="tabular-nums">
          {plural(model.tables.size, "table")} · {plural(declared, "key")}
        </span>
      )}
      <span className="flex items-center gap-1 text-dim">
        {map.status.kind === "refreshing" ? "Refreshing…" : fetched !== null ? `Refreshed ${timeAgo(now, fetched)}` : null}
        <IconButton aria-label="Refresh map" title="Refresh catalog" className="size-5" onClick={map.actions.refresh}>
          <RefreshCwIcon size={11} />
        </IconButton>
      </span>
    </div>
  );
}

export async function browseRows(map: SchemaMapView, table: ErTable): Promise<void> {
  if (map.db) await openTableTab({ db: map.db, schema: table.schema, relation: table.name }, undefined, map.tab.id);
}

export async function queryInNewCell(map: SchemaMapView, table: ErTable): Promise<void> {
  if (!map.db) return;
  const text = await browseTemplate(map.db, table.schema, table.name);
  if (text !== null) openSqlTab({ db: map.db, schema: map.tab.console.schema ?? null, text, after: map.tab.id });
}

export function FocusStrip({ map }: { map: SchemaMapView }) {
  const focus = map.focus!;
  const { table, counts } = focus;
  const db = map.db;
  return (
    <div data-testid="map-focus-strip" className="flex min-h-11 items-center gap-2.5 px-3 py-1.5 text-ui-11 whitespace-nowrap text-muted">
      <span className="flex min-w-0 shrink items-center gap-2">
        <Table2Icon size={14} className="shrink-0 text-keyword" />
        <span className="shrink-0 font-mono text-ui-12.5 font-semibold text-fg">{table.name}</span>
        <span className="min-w-0 truncate text-dim">
          {plural(table.columns.length, "column")}
          {table.rowEstimate !== null && ` · ~${compactCount(table.rowEstimate)} rows`}
        </span>
      </span>
      <span data-testid="map-focus-counts" className="flex shrink-0 items-center gap-1">
        References <b className="font-semibold text-fg">{counts.references}</b>
        <span className="text-dim">·</span>
        Referenced by <b className="font-semibold text-fg">{counts.referencedBy}</b>
        <span className="text-dim">·</span>
        Possible <b className="font-semibold text-amber">{counts.possible}</b>
      </span>
      <PartialMapNote model={map.model} short />
      <span className="flex-1" />
      <button
        type="button"
        className={cx(TOOL, "border-transparent bg-run text-run-fg hover:bg-run-hover disabled:opacity-40")}
        disabled={!db}
        onClick={() => void browseRows(map, table)}
      >
        <PlayIcon size={12} />
        Browse rows
      </button>
      <button type="button" className={cx(TOOL, "text-fg disabled:opacity-40")} disabled={!db} onClick={() => void queryInNewCell(map, table)}>
        <PlusIcon size={13} />
        Query in new cell
      </button>
    </div>
  );
}

function CanvasNote({ children, tone = "muted" }: { children: ReactNode; tone?: "muted" | "error" }) {
  return (
    <div role="status" className="pointer-events-none absolute inset-0 grid place-items-center p-6 text-center">
      <span className={cx("pointer-events-auto flex flex-col items-center gap-2 text-ui-12", tone === "error" ? "text-danger" : "text-muted")}>{children}</span>
    </div>
  );
}

export function findShortcut(e: KeyboardEvent, find: RefObject<HTMLInputElement | null>): boolean {
  if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "f") return false;
  e.preventDefault();
  find.current?.focus();
  find.current?.select();
  return true;
}

export function CanvasStatus({ map }: { map: SchemaMapView }) {
  const { status, model } = map;
  const db = map.db?.database;
  return (
    <>
      {status.kind === "unconnected" && <CanvasNote>Connect to {db ?? "a database"} to draw this schema's map.</CanvasNote>}
      {status.kind === "loading" && (
        <CanvasNote>
          <span className="flex items-center gap-2">
            <Spinner /> Loading the schema map…
          </span>
        </CanvasNote>
      )}
      {status.kind === "error" && !model && (
        <CanvasNote tone="error">
          {status.message}
          <Button className="h-7 text-ui-12" onClick={map.actions.refresh}>
            Retry
          </Button>
        </CanvasNote>
      )}
      {status.kind === "error" && model && (
        <p role="status" className="pointer-events-none absolute top-3 left-3 text-ui-11.5 text-danger">
          Could not refresh: {status.message}
        </p>
      )}
      {model && model.tables.size === 0 && <CanvasNote>No tables in {model.schema}.</CanvasNote>}
    </>
  );
}

export function MapCellBody({ map, height }: { map: SchemaMapView; height: number }) {
  const find = useRef<HTMLInputElement>(null);
  return (
    <div className="border-t border-border" onKeyDown={(e) => findShortcut(e, find)}>
      <MapToolbar map={map} findRef={find} />
      <div className="relative">
        <SchemaCanvas
          map={map}
          mode={{ opens: "fitWidth", wheel: "scrollsPage" }}
          height={height}
          testId={`map-cell-${map.tab.id}`}
          label={`Schema map of ${map.schema ?? "the default schema"}`}
        />
        <CanvasStatus map={map} />
      </div>
      <div className="border-t border-border">{map.focus ? <FocusStrip map={map} /> : <MapLegend map={map} />}</div>
    </div>
  );
}
