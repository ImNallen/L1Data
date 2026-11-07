import { useMemo, useRef } from "react";
import { ConnectionTree } from "../features/sidebar/ConnectionTree";
import { SidebarSearch } from "../features/sidebar/SidebarSearch";
import { filterTree } from "../features/sidebar/treeFilter";
import { focusFirstRow } from "../features/sidebar/treeFocus";
import { useWorkspace } from "../state/workspace";
import { useLive } from "../state/live";
import { useCatalog } from "../state/catalog";
import { parseQuery } from "../ui/textMatch";

/** Exercises the tree's search and keyboard contract independently of the split sidebar. */
export function ConnectionTreeHarness() {
  const query = useWorkspace((s) => s.treeQuery);
  const configs = useWorkspace((s) => s.configs);
  const connections = useLive((s) => s.connections);
  const views = useCatalog((s) => s.views);
  const tree = useRef<HTMLDivElement>(null);
  const filter = useMemo(() => {
    const tokens = parseQuery(query);
    return tokens.length ? filterTree(tokens, configs, connections, views) : null;
  }, [query, configs, connections, views]);
  return (
    <>
      <SidebarSearch
        testId="sidebar-search"
        scope="workspace"
        query={query}
        count={filter?.count ?? 0}
        onQueryChange={useWorkspace.getState().setTreeQuery}
        onArrowDown={() => focusFirstRow(tree.current)}
      />
      <button type="button" data-testid="before-tree">
        Before tree
      </button>
      <ConnectionTree ref={tree} filter={filter} query={query} />
      <button type="button" data-testid="after-tree">
        After tree
      </button>
    </>
  );
}
