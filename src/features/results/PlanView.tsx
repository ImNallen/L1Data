import { ChevronRightIcon } from "lucide-react";
import { Fragment, useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { commands, type Plan, type PlanBuffers, type PlanNode } from "../../ipc/bindings";
import { refKey, type RowResult } from "../../state/status";
import { treeKey, type TreeItem } from "../sidebar/treeKeys";
import { cx } from "../../ui/primitives";

const INDENT = 12;

const BUFFER_FIELDS: { key: keyof PlanBuffers; label: string }[] = [
  { key: "sharedHit", label: "Shared hit" },
  { key: "sharedRead", label: "Shared read" },
  { key: "sharedDirtied", label: "Shared dirtied" },
  { key: "sharedWritten", label: "Shared written" },
  { key: "localHit", label: "Local hit" },
  { key: "localRead", label: "Local read" },
  { key: "localDirtied", label: "Local dirtied" },
  { key: "localWritten", label: "Local written" },
  { key: "tempRead", label: "Temp read" },
  { key: "tempWritten", label: "Temp written" },
];

type Flat = { id: string; node: PlanNode; level: number; statement: number };

type TreeState = { key: string; collapsed: Set<string>; focusId: string };

function msText(ms: number): string {
  return `${Number(ms.toFixed(3))} ms`;
}

function countText(n: number): string {
  return Number.isInteger(n) ? String(n) : String(Number(n.toFixed(2)));
}

function flatten(plan: Plan, collapsed: ReadonlySet<string>): Flat[] {
  const out: Flat[] = [];
  const walk = (node: PlanNode, level: number, id: string, statement: number) => {
    out.push({ id, node, level, statement });
    if (node.children.length === 0 || collapsed.has(id)) return;
    node.children.forEach((child, i) => walk(child, level + 1, `${id}/${i}`, statement));
  };
  plan.statements.forEach((statement, index) => walk(statement.root, 1, String(index), index));
  return out;
}

export function PlanView({ result }: { result: RowResult }) {
  const ref = result.meta.ref;
  const key = refKey(ref);
  const [loaded, setLoaded] = useState<{ key: string; plan?: Plan; error?: string } | null>(null);
  const [tree, setTree] = useState<TreeState>({ key: "", collapsed: new Set(), focusId: "0" });
  const marked = useRef("");
  const treeRef = useRef<HTMLDivElement>(null);
  const nodeIds = useId();

  const active: TreeState = tree.key === key ? tree : { key, collapsed: new Set(), focusId: "0" };
  if (tree.key !== key) setTree(active);

  useEffect(() => {
    let cancelled = false;
    void commands.resultPlan(ref).then(
      (res) => {
        if (cancelled) return;
        if (res.status === "ok") {
          performance.mark("plan-data");
          setLoaded({ key, plan: res.data });
        } else setLoaded({ key, error: res.error.kind === "NotAPlan" ? res.error.what : "The plan could not be read." });
      },
      () => {
        if (!cancelled) setLoaded({ key, error: "The plan could not be read." });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [ref, key]);

  const plan = loaded?.key === key ? loaded.plan : undefined;
  const error = loaded?.key === key ? loaded.error : undefined;
  const rows = plan ? flatten(plan, active.collapsed) : [];
  const found = rows.findIndex((row) => row.id === active.focusId);
  const at = found === -1 ? 0 : found;

  useEffect(() => {
    if (!plan || marked.current === key) return;
    marked.current = key;
    performance.mark("plan-paint");
    if (performance.getEntriesByName("plan-data").length > 0) performance.measure("plan-paint-ms", "plan-data", "plan-paint");
  }, [plan, key]);

  useEffect(() => {
    if (!plan) return;
    treeRef.current?.querySelector("[data-focused='true']")?.scrollIntoView({ block: "nearest" });
  }, [plan, at, active.collapsed]);

  if (error) {
    return (
      <p data-testid="plan-error" className="px-3 py-2 text-ui-13 text-danger">
        {error}
      </p>
    );
  }
  if (!plan) {
    return <p className="px-3 py-2 text-ui-13 text-dim">Reading the plan…</p>;
  }
  const focused = rows[at];
  const statement = focused ? plan.statements[focused.statement] : plan.statements[0];

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
    const items: TreeItem[] = rows.map((row) => ({
      level: row.level,
      expandable: row.node.children.length > 0,
      expanded: row.node.children.length > 0 && !active.collapsed.has(row.id),
    }));
    const action = treeKey(event.key, items, at);
    if (!action || !focused) return;
    event.preventDefault();
    if (action.kind === "focus") {
      const next = rows[action.index];
      if (next) setTree({ key, collapsed: active.collapsed, focusId: next.id });
      return;
    }
    const collapsed = new Set(active.collapsed);
    if (action.kind === "expand") collapsed.delete(focused.id);
    else if (action.kind === "collapse") collapsed.add(focused.id);
    else if (collapsed.has(focused.id)) collapsed.delete(focused.id);
    else collapsed.add(focused.id);
    const visible = flatten(plan, collapsed);
    const still = visible.some((row) => row.id === focused.id);
    setTree({ key, collapsed, focusId: still ? focused.id : (visible[Math.min(at, visible.length - 1)]?.id ?? "0") });
  };

  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-w-0 flex-1 flex-col">
        <div data-testid="plan-header" className="flex h-8 shrink-0 items-center gap-3 border-b border-grid px-3 text-ui-12.5 text-muted">
          {statement?.planningTimeMs != null && <span>Planning {msText(statement.planningTimeMs)}</span>}
          {statement?.executionTimeMs != null && <span>Execution {msText(statement.executionTimeMs)}</span>}
        </div>
        {/* Focus stays on the tree. The focused row is its active descendant and shows the focus ring in its place. */}
        <div
          ref={treeRef}
          role="tree"
          tabIndex={0}
          aria-label="Query plan"
          aria-activedescendant={focused ? `${nodeIds}-${at}` : undefined}
          onKeyDown={onKeyDown}
          className="group min-h-0 flex-1 overflow-auto py-1 outline-none"
        >
          {rows.map((row, index) => {
            const expandable = row.node.children.length > 0;
            const expanded = expandable && !active.collapsed.has(row.id);
            const root = plan.statements[row.statement]?.root;
            const ratio = root?.totalTimeMs != null && root.totalTimeMs > 0 && row.node.selfTimeMs != null ? row.node.selfTimeMs / root.totalTimeMs : null;
            const statementText = row.id === String(row.statement) ? (plan.statements[row.statement]?.statementText ?? "") : "";
            return (
              <Fragment key={row.id}>
                {statementText !== "" && (
                  <p data-testid={`plan-statement-${row.statement}`} className="truncate px-3 pt-1.5 font-mono text-ui-12.5 text-fg">
                    {statementText}
                  </p>
                )}
                <div
                  id={`${nodeIds}-${index}`}
                  role="treeitem"
                  aria-selected={index === at}
                  aria-level={row.level}
                  aria-expanded={expandable ? expanded : undefined}
                  data-testid={`plan-node-${index}`}
                  data-focused={index === at ? "true" : undefined}
                  style={{ paddingLeft: 8 + (row.level - 1) * INDENT }}
                  onClick={() => {
                    setTree({ key, collapsed: active.collapsed, focusId: row.id });
                    treeRef.current?.focus();
                  }}
                  className={cx(
                    "flex h-7 cursor-default items-center gap-2 pr-3 text-ui-13",
                    index === at ? "bg-selected group-focus-visible:ring-1 group-focus-visible:ring-focus group-focus-visible:ring-inset" : "hover:bg-hover",
                  )}
                >
                  <span className="grid w-3.5 shrink-0 place-items-center text-muted">
                    {expandable && <ChevronRightIcon size={14} className={cx("transition-transform duration-100", expanded && "rotate-90")} />}
                  </span>
                  <span className="truncate text-fg">{row.node.operation}</span>
                  {row.node.detail && <span className="min-w-0 truncate text-muted">{row.node.detail}</span>}
                  <span className="shrink-0 tabular-nums text-muted">
                    {row.node.totalCost != null && <span>cost {countText(row.node.totalCost)}</span>}
                    {row.node.planRows != null && <span> est {countText(row.node.planRows)}</span>}
                    {row.node.actualRows != null && <span> actual {countText(row.node.actualRows)}</span>}
                    {row.node.totalTimeMs != null && <span> {msText(row.node.totalTimeMs)}</span>}
                  </span>
                  {row.node.diverges && (
                    <span data-testid="plan-diverges" className="shrink-0 font-medium text-amber">
                      10×
                    </span>
                  )}
                  {ratio != null && (
                    <span data-testid="plan-self-bar" className="relative h-1.5 w-16 shrink-0 overflow-hidden rounded-full bg-control">
                      <span className="absolute inset-y-0 left-0 bg-run" style={{ width: `${Math.min(100, ratio * 100)}%` }} />
                    </span>
                  )}
                  {row.node.selfTimeMs != null && <span className="ml-auto shrink-0 tabular-nums text-muted">{msText(row.node.selfTimeMs)}</span>}
                </div>
              </Fragment>
            );
          })}
        </div>
      </div>
      {focused && (
        <aside data-testid="plan-detail" className="w-64 shrink-0 overflow-auto border-l border-grid p-3 text-ui-12.5">
          <div className="font-medium text-fg">{focused.node.operation}</div>
          {focused.node.detail && <div className="mt-0.5 text-muted">{focused.node.detail}</div>}
          {focused.node.properties.length > 0 && (
            <dl className="mt-3 space-y-2">
              {focused.node.properties.map((property, index) => (
                <div key={`${property.name}-${index}`}>
                  <dt className="text-dim">{property.name}</dt>
                  <dd className="font-mono break-all text-fg">{property.value}</dd>
                </div>
              ))}
            </dl>
          )}
          {focused.node.buffers && (
            <dl className="mt-3 space-y-1">
              <div className="text-dim">Buffers</div>
              {BUFFER_FIELDS.map((field) => (
                <div key={field.key} className="flex justify-between gap-3">
                  <dt className="text-muted">{field.label}</dt>
                  <dd className="tabular-nums text-fg">{focused.node.buffers?.[field.key]}</dd>
                </div>
              ))}
            </dl>
          )}
        </aside>
      )}
    </div>
  );
}
