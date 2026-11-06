import { BookmarkIcon, ChartNoAxesColumnIcon, ClockIcon, PlusIcon, SlidersHorizontalIcon } from "lucide-react";
import { useMemo, useRef } from "react";
import { useCatalog } from "../../state/catalog";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { Button, cx, IconButton } from "../../ui/primitives";
import { parseQuery } from "../../ui/textMatch";
import { isMac, type HostedWindow } from "../../shell/hostedWindow";
import { SidebarToggle, windowLeadingInset } from "../../shell/WindowEdgeActions";
import { openTableTab } from "../editor/runActions";
import { UpdateButton } from "../updates/UpdateButton";
import { ConnectionTree } from "./ConnectionTree";
import { SidebarSearch } from "./SidebarSearch";
import { focusFirstRow } from "./treeFocus";
import { filterTree, firstRelation, type TreeFilter } from "./treeFilter";

function SidebarHeader({ hosted }: { hosted: HostedWindow | null }) {
  return (
    <div data-tauri-drag-region="deep" className={cx("flex h-[52px] shrink-0 items-center gap-2 pr-3", windowLeadingInset(hosted))}>
      <SidebarToggle />
      <span data-testid="app-title" className="min-w-0 flex-1 truncate text-ui-14 tracking-[-0.01em] text-tree">
        <span className="font-semibold">L1</span> <span className="font-normal text-muted">Data</span>
      </span>
    </div>
  );
}

export function Sidebar({ hosted }: { hosted: HostedWindow | null }) {
  return (
    <aside data-testid="sidebar-root" className="flex h-full flex-col bg-sidebar">
      <SidebarHeader hosted={hosted} />
      <WorkspaceSidebarBody />
    </aside>
  );
}

function useTreeFilter(query: string): TreeFilter | null {
  const configs = useWorkspace((s) => s.configs);
  const connections = useLive((s) => s.connections);
  const views = useCatalog((s) => s.views);
  return useMemo(() => {
    const tokens = parseQuery(query);
    return tokens.length > 0 ? filterTree(tokens, configs, connections, views) : null;
  }, [query, configs, connections, views]);
}

function WorkspaceSidebarBody() {
  const firstConnection = useWorkspace((s) => Object.keys(s.configs).length === 0);
  const open = useWorkspace((s) => s.open);
  const openSettings = useWorkspace((s) => s.openSettings);
  const query = useWorkspace((s) => s.treeQuery);
  const setTreeQuery = useWorkspace((s) => s.setTreeQuery);
  const filter = useTreeFilter(query);
  const tree = useRef<HTMLDivElement>(null);
  const openFirst = () => {
    const first = filter && firstRelation(filter, parseQuery(query));
    if (first) openTableTab(first);
  };

  return (
    <>
      <SidebarSearch
        testId="sidebar-search"
        scope="workspace"
        query={query}
        count={filter?.count ?? 0}
        onQueryChange={setTreeQuery}
        onEnter={openFirst}
        onArrowDown={() => focusFirstRow(tree.current)}
      >
        <IconButton
          className="size-[34px] shrink-0"
          data-testid="new-connection"
          aria-label="New connection"
          onClick={() => open({ kind: "connection", editing: null })}
        >
          <PlusIcon size={17} />
        </IconButton>
      </SidebarSearch>

      {firstConnection ? (
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-6">
          <h2 className="text-ui-14 font-semibold text-fg">Connect a database</h2>
          <p className="mt-2 text-ui-13 leading-relaxed text-dim">Add a PostgreSQL or SQL Server connection to browse tables and run SQL.</p>
          <Button className="mt-4" onClick={() => open({ kind: "connection", editing: null })}>
            Connect a database
          </Button>
        </div>
      ) : (
        <ConnectionTree ref={tree} filter={filter} query={query} />
      )}

      <div className="mx-2 flex shrink-0 items-center gap-1 py-2">
        <IconButton className="size-[34px] shrink-0" data-testid="open-settings" aria-label="Settings" title="Settings" onClick={() => openSettings()}>
          <SlidersHorizontalIcon size={16} />
        </IconButton>
        <IconButton className="size-[34px] shrink-0" data-testid="open-usage" aria-label="Usage" title="Usage" onClick={() => open({ kind: "usage" })}>
          <ChartNoAxesColumnIcon size={16} />
        </IconButton>
        <IconButton
          className="size-[34px] shrink-0"
          data-testid="open-history"
          aria-label="Query history"
          title="Query history"
          onClick={() => open({ kind: "history" })}
        >
          <ClockIcon size={16} />
        </IconButton>
        <IconButton
          className="size-[34px] shrink-0"
          data-testid="open-saved-queries"
          aria-label="Saved queries"
          title={`Saved queries (${isMac(navigator.userAgent) ? "⌘E" : "Ctrl+E"})`}
          onClick={() => open({ kind: "saved", entry: { kind: "browse" } })}
        >
          <BookmarkIcon size={16} />
        </IconButton>
        <UpdateButton />
      </div>
    </>
  );
}
