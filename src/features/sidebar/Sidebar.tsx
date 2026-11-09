import * as Menu from "@radix-ui/react-dropdown-menu";
import { BookmarkIcon, ChartNoAxesColumnIcon, ClockIcon, DatabaseIcon, PlusIcon, SlidersHorizontalIcon } from "lucide-react";
import { useMemo, useRef, useState, type KeyboardEvent, type PointerEvent, type RefObject } from "react";
import { isViewKind, useCatalog, viewFor } from "../../state/catalog";
import { useLive } from "../../state/live";
import { shownDb, useWorkspace } from "../../state/workspace";
import { cx, IconButton, MenuContent, MenuItem } from "../../ui/primitives";
import { parseQuery } from "../../ui/textMatch";
import type { HostedWindow } from "../../shell/hostedWindow";
import { SidebarToggle, windowLeadingInset } from "../../shell/WindowEdgeActions";
import { openTableTab } from "../editor/runActions";
import { UpdateButton } from "../updates/UpdateButton";
import { ConnectionTree } from "./ConnectionTree";
import { SidebarSearch } from "./SidebarSearch";
import { focusFirstRow } from "./treeFocus";
import { filterTree, firstRelation, type TreeFilter, type ObjectFilterScope } from "./treeFilter";

function useTreeFilter(query: string, scope?: ObjectFilterScope | null): TreeFilter | null {
  const configs = useWorkspace((s) => s.configs);
  const connections = useLive((s) => s.connections);
  const views = useCatalog((s) => s.views);
  return useMemo(() => {
    const tokens = parseQuery(query);
    return tokens.length > 0 && scope !== null ? filterTree(tokens, configs, connections, views, scope) : null;
  }, [query, configs, connections, views, scope]);
}

type Category = "tables" | "views" | "all";

/**
 * Tables, Views, or All. The choice is a lighter thumb set into a darker track, and the thumb slides to the next choice.
 * The reduced-motion setting stops the slide (index.css).
 */
function CategorySwitch({ value, counts, onChange }: { value: Category; counts: Record<Category, number>; onChange: (c: Category) => void }) {
  const items: [Category, string][] = [
    ["tables", "Tables"],
    ["views", "Views"],
    ["all", "All"],
  ];
  const index = items.findIndex(([c]) => c === value);
  return (
    <div className="relative mx-3 grid h-[30px] shrink-0 grid-cols-3 rounded-lg bg-segment-track p-[3px] shadow-[inset_0_1px_2px_rgb(0_0_0/0.06)]">
      <span
        aria-hidden
        data-testid="category-thumb"
        className="absolute inset-y-[3px] left-[3px] w-[calc((100%-6px)/3)] rounded-md border-[0.5px] border-segment-thumb-border bg-segment-thumb shadow-[0_1px_2px_rgb(0_0_0/0.08)] transition-transform duration-[220ms] ease-out"
        style={{ transform: `translateX(${index * 100}%)` }}
      />
      {items.map(([c, label]) => (
        <button
          key={c}
          type="button"
          aria-pressed={c === value}
          onClick={() => onChange(c)}
          className={cx(
            "relative flex min-w-0 items-center justify-center gap-1.5 rounded-md text-ui-11.5 transition-colors",
            c === value ? "font-medium text-fg" : "text-muted hover:text-fg",
          )}
        >
          {label}
          <span className="text-ui-10.5 font-normal text-dim tabular-nums">{counts[c]}</span>
        </button>
      ))}
    </div>
  );
}

const MIN_HALF = 140;

/**
 * The border between the databases and the objects is the resize handle: it turns blue on hover. Drag it, or focus it
 * and press the arrow keys, to share the height differently. Double-click returns to an even split.
 */
function SplitHandle({ container }: { container: RefObject<HTMLElement | null> }) {
  const split = useWorkspace((s) => s.layout.sidebarSplit);
  const setLayout = useWorkspace((s) => s.setLayout);
  const [dragging, setDragging] = useState(false);
  const bounds = () => {
    const height = container.current?.clientHeight ?? 0;
    const min = height > 2 * MIN_HALF ? (MIN_HALF / height) * 100 : 20;
    return [min, 100 - min] as const;
  };
  const set = (percent: number) => {
    const [min, max] = bounds();
    setLayout({ sidebarSplit: Math.round(Math.min(max, Math.max(min, percent))) });
  };
  const fromPointer = (e: PointerEvent<HTMLDivElement>) => {
    const rect = container.current?.getBoundingClientRect();
    if (rect && rect.height > 0) set(((e.clientY - rect.top) / rect.height) * 100);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowUp: -5, ArrowDown: 5 }[e.key];
    if (step !== undefined) set(split + step);
    else if (e.key === "Home") set(0);
    else if (e.key === "End") set(100);
    else return;
    e.preventDefault();
  };
  const [min, max] = bounds();
  return (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation="horizontal"
      aria-label="Resize database list"
      aria-valuenow={split}
      aria-valuemin={Math.round(min)}
      aria-valuemax={Math.round(max)}
      data-testid="sidebar-split"
      data-dragging={dragging || undefined}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        setDragging(true);
      }}
      onPointerMove={(e) => dragging && fromPointer(e)}
      onPointerUp={(e) => {
        e.currentTarget.releasePointerCapture(e.pointerId);
        setDragging(false);
      }}
      onPointerCancel={() => setDragging(false)}
      onKeyDown={onKeyDown}
      onDoubleClick={() => set(50)}
      className="relative z-10 h-px shrink-0 cursor-row-resize bg-border outline-none transition-[background-color,scale] duration-100 after:absolute after:inset-x-0 after:-inset-y-1 hover:scale-y-200 hover:bg-run focus-visible:scale-y-200 focus-visible:bg-focus data-dragging:scale-y-200 data-dragging:bg-run"
    />
  );
}

/** Before the first connection, the database list is the first step. */
function FirstConnection({ onAdd }: { onAdd: () => void }) {
  return (
    <div className="mx-3 mt-3 flex flex-col items-center gap-2 rounded-lg border border-dashed border-control px-4 py-5 text-center">
      <DatabaseIcon size={20} className="text-dim" />
      <h2 className="text-ui-13 font-semibold">Connect a database</h2>
      <p className="text-ui-11.5 leading-relaxed text-dim">Add a PostgreSQL or SQL Server connection to begin.</p>
      <button type="button" onClick={onAdd} className="mt-1 h-7 rounded-md bg-run px-3 text-ui-12 font-medium text-run-fg hover:bg-run-hover">
        New connection
      </button>
    </div>
  );
}

export function Sidebar({ hosted }: { hosted: HostedWindow | null }) {
  const configs = useWorkspace((s) => s.configs);
  const shown = useWorkspace(shownDb);
  const inNotebook = useWorkspace((s) => s.shown.kind === "notebook");
  const showDatabase = useWorkspace((s) => s.showDatabase);
  const open = useWorkspace((s) => s.open);
  const openSettings = useWorkspace((s) => s.openSettings);
  const query = useWorkspace((s) => s.treeQuery);
  const setTreeQuery = useWorkspace((s) => s.setTreeQuery);
  const [objectQuery, setObjectQuery] = useState("");
  const [category, setCategory] = useState<Category>("tables");
  const split = useWorkspace((s) => s.layout.sidebarSplit);
  const root = useRef<HTMLElement>(null);
  const first = Object.values(configs)[0];
  // The start commands with no database chosen browse the first connection's database. A notebook with no connection browses none.
  const db = useMemo(() => shown ?? (!inNotebook && first ? { connectionId: first.id, database: first.database } : null), [shown, inNotebook, first]);
  const catalog = useCatalog((s) => viewFor(s.views, db)?.catalog);
  const databaseScope = useMemo(() => ({ kind: "databases" as const, selected: db, select: showDatabase }), [db, showDatabase]);
  const objectScope = useMemo(() => db && { kind: "objects" as const, db, category }, [db, category]);
  const filter = useTreeFilter(query);
  const objectFilter = useTreeFilter(objectQuery, objectScope);
  const menuTrigger = useRef<HTMLButtonElement>(null);
  const fromMenu = (action: () => void) => {
    menuTrigger.current?.focus();
    action();
  };
  const tree = useRef<HTMLDivElement>(null);
  const objects = useRef<HTMLDivElement>(null);
  const openFirst = () => {
    const found = objectFilter && firstRelation(objectFilter, parseQuery(objectQuery));
    if (found) openTableTab(found);
  };
  const schemas = catalog?.schemas ?? [];
  const relations = schemas.flatMap((s) => s.relations);
  const tables = relations.filter((r) => !isViewKind(r.kind)).length;
  const others = schemas.reduce((n, s) => n + s.functions.length + s.sequences.length + s.types.length + s.synonyms.length, catalog?.extensions.length ?? 0);
  const counts = { tables, views: relations.length - tables, all: relations.length + others };
  return (
    <aside ref={root} data-testid="sidebar-root" className="database-sidebar flex h-full flex-col bg-sidebar">
      <section className="flex min-h-0 flex-col" style={{ height: `${split}%` }}>
        <div
          data-testid="sidebar-header"
          data-tauri-drag-region="deep"
          className={cx("flex h-14 shrink-0 items-center gap-1 pr-3", windowLeadingInset(hosted))}
        >
          <SidebarToggle />
          <div className="flex-1" />
          <IconButton
            data-testid="new-connection"
            aria-label="New connection"
            title="New connection"
            onClick={() => open({ kind: "connection", editing: null })}
          >
            <PlusIcon size={15} />
          </IconButton>
          <Menu.Root modal={false}>
            <Menu.Trigger asChild>
              <IconButton ref={menuTrigger} aria-label="Workspace options" title="Workspace options">
                <SlidersHorizontalIcon size={15} />
              </IconButton>
            </Menu.Trigger>
            <MenuContent align="end">
              <MenuItem testId="open-settings" onSelect={() => fromMenu(() => openSettings())}>
                <SlidersHorizontalIcon size={14} />
                Settings
              </MenuItem>
              <MenuItem testId="open-usage" onSelect={() => fromMenu(() => open({ kind: "usage" }))}>
                <ChartNoAxesColumnIcon size={14} />
                Usage
              </MenuItem>
              <MenuItem testId="open-history" onSelect={() => fromMenu(() => open({ kind: "history" }))}>
                <ClockIcon size={14} />
                Query history
              </MenuItem>
              <MenuItem testId="open-saved-queries" onSelect={() => fromMenu(() => open({ kind: "saved", entry: { kind: "browse" } }))}>
                <BookmarkIcon size={14} />
                Saved queries
              </MenuItem>
            </MenuContent>
          </Menu.Root>
          <UpdateButton />
        </div>
        {first ? (
          <>
            <SidebarSearch
              showCount={false}
              testId="sidebar-search"
              scope="workspace"
              placeholder="Find a database…"
              query={query}
              count={filter?.count ?? 0}
              onQueryChange={setTreeQuery}
              onArrowDown={() => focusFirstRow(tree.current)}
            />
            <ConnectionTree ref={tree} filter={filter} query={query} scope={databaseScope} />
          </>
        ) : (
          <FirstConnection onAdd={() => open({ kind: "connection", editing: null })} />
        )}
      </section>
      <SplitHandle container={root} />
      <section className="flex min-h-0 flex-1 flex-col">
        <div className="flex h-[42px] shrink-0 items-center gap-2 px-[18px]">
          {db && <DatabaseIcon size={14} className="shrink-0 text-keyword" />}
          <h2 className="min-w-0 shrink truncate text-ui-13 font-semibold">{db?.database ?? "Database objects"}</h2>
          {db && <span className="min-w-0 flex-1 truncate text-ui-11 text-dim">on {configs[db.connectionId]?.name}</span>}
        </div>
        {objectScope ? (
          <>
            <SidebarSearch
              testId="object-search"
              scope="workspace"
              placeholder="Filter tables, views…"
              query={objectQuery}
              count={objectFilter?.count ?? 0}
              onQueryChange={setObjectQuery}
              onEnter={openFirst}
              onArrowDown={() => focusFirstRow(objects.current)}
            />
            <div className="h-2 shrink-0" />
            <CategorySwitch value={category} counts={counts} onChange={setCategory} />
            <ConnectionTree ref={objects} filter={objectFilter} query={objectQuery} scope={objectScope} />
          </>
        ) : (
          <p className="px-6 pt-12 text-center text-ui-11.5 leading-relaxed text-dim">Choose a database to browse its tables, views, and routines.</p>
        )}
      </section>
    </aside>
  );
}
