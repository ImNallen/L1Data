import * as Menu from "@radix-ui/react-dropdown-menu";
import { BookmarkIcon, ChartNoAxesColumnIcon, ClockIcon, PlusIcon, SlidersHorizontalIcon, ChevronDownIcon } from "lucide-react";
import { useCallback, useMemo, useRef, useState } from "react";
import type { DatabaseRef } from "../../ipc/bindings";
import { isViewKind, useCatalog, viewFor } from "../../state/catalog";
import { useLive } from "../../state/live";
import { tabTarget, useWorkspace } from "../../state/workspace";
import { Button, cx, IconButton, MenuContent, MenuItem } from "../../ui/primitives";
import { parseQuery } from "../../ui/textMatch";
import type { HostedWindow } from "../../shell/hostedWindow";
import { SidebarToggle, windowLeadingInset } from "../../shell/WindowEdgeActions";
import { openTableTab, setTabTarget } from "../editor/runActions";
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

export function Sidebar({ hosted }: { hosted: HostedWindow | null }) {
  const configs = useWorkspace((s) => s.configs);
  const active = useWorkspace((s) => s.active);
  const target = useWorkspace((s) => tabTarget(s.active ? s.tabs[s.active] : undefined));
  const open = useWorkspace((s) => s.open);
  const openSettings = useWorkspace((s) => s.openSettings);
  const query = useWorkspace((s) => s.treeQuery);
  const setTreeQuery = useWorkspace((s) => s.setTreeQuery);
  const [picked, setPicked] = useState<DatabaseRef | null>(null);
  const [objectQuery, setObjectQuery] = useState("");
  const [category, setCategory] = useState<"tables" | "views" | "all">("tables");
  const first = Object.values(configs)[0];
  const db = useMemo(
    () => (active ? target : (picked ?? (first ? { connectionId: first.id, database: first.database } : null))),
    [picked, active, target, first],
  );
  const catalog = useCatalog((s) => viewFor(s.views, db)?.catalog);
  const selectDatabase = useCallback(
    (db: DatabaseRef) => {
      if (active) void setTabTarget(active, db);
      else setPicked(db);
    },
    [active],
  );
  const databaseScope = useMemo(() => ({ kind: "databases" as const, selected: db, select: selectDatabase }), [db, selectDatabase]);
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
  const relations = catalog?.schemas.flatMap((s) => s.relations) ?? [];
  const tableCount = relations.filter((r) => !isViewKind(r.kind)).length;
  const viewCount = relations.length - tableCount;
  return (
    <aside data-testid="sidebar-root" className="database-sidebar flex h-full flex-col bg-sidebar">
      <section className="flex min-h-0 flex-1 flex-col">
        <div
          data-testid="sidebar-header"
          data-tauri-drag-region="deep"
          className={cx("flex h-12 shrink-0 items-center gap-1 pr-3", windowLeadingInset(hosted))}
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
        {first ? (
          <ConnectionTree ref={tree} filter={filter} query={query} scope={databaseScope} />
        ) : (
          <div className="min-h-0 flex-1 overflow-auto px-4 py-6">
            <h2 className="text-ui-13 font-medium">Connect a database</h2>
            <p className="mt-2 text-ui-12 leading-relaxed text-dim">Add a PostgreSQL or SQL Server connection to begin.</p>
            <Button className="mt-4" onClick={() => open({ kind: "connection", editing: null })}>
              Connect a database
            </Button>
          </div>
        )}
      </section>
      <section className="flex min-h-0 flex-1 flex-col border-t border-control">
        <div className="flex h-11 shrink-0 items-center gap-2 px-4">
          <h2 className="min-w-0 flex-1 truncate text-ui-13 font-semibold">{db?.database ?? "Database objects"}</h2>
          <span className="max-w-1/2 truncate text-ui-10 text-dim">{db ? configs[db.connectionId]?.name : ""}</span>
        </div>
        <SidebarSearch
          showCount={false}
          testId="object-search"
          scope="workspace"
          placeholder="Filter tables, views…"
          query={objectQuery}
          count={objectFilter?.count ?? 0}
          onQueryChange={setObjectQuery}
          onEnter={openFirst}
          onArrowDown={() => focusFirstRow(objects.current)}
        />
        <div className="flex h-9 shrink-0 items-center gap-3 px-4 text-ui-11">
          <button
            type="button"
            aria-pressed={category === "tables"}
            className={category === "tables" ? "text-keyword" : "text-muted"}
            onClick={() => setCategory("tables")}
          >
            Tables {tableCount}
          </button>
          <button
            type="button"
            aria-pressed={category === "views"}
            className={category === "views" ? "text-keyword" : "text-muted"}
            onClick={() => setCategory("views")}
          >
            Views {viewCount}
          </button>
          <button
            type="button"
            aria-pressed={category === "all"}
            className={cx("ml-auto flex items-center gap-1", category === "all" ? "text-keyword" : "text-muted")}
            onClick={() => setCategory("all")}
          >
            More
            <ChevronDownIcon size={11} />
          </button>
        </div>
        {objectScope ? (
          <ConnectionTree ref={objects} filter={objectFilter} query={objectQuery} scope={objectScope} />
        ) : (
          <p className="px-4 py-3 text-ui-12 text-dim">Choose a database to browse its objects.</p>
        )}
      </section>
    </aside>
  );
}
