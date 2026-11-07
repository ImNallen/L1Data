import { useEffect, useRef, useState } from "react";
import { commands, type DatabaseRef, type RowPage } from "../../ipc/bindings";
import { configureErrorText, runErrorText } from "../../ipc/errors";
import { useWorkspace } from "../../state/workspace";
import { sameDb } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { useNow } from "../../ui/useNow";
import { rowResults, runSummary } from "../../state/status";

export function useInlineQuery(sql: string, db: DatabaseRef | null) {
  const [id] = useState(() => crypto.randomUUID());
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);
  const console = useLive((s) => s.consoles[id]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      void commands.consoleClose(id, true).catch(() => {});
    };
  }, [id]);
  const run = async () => {
    if (!db || starting || console?.work) return;
    setStarting(true);
    setError(null);
    try {
      const workspace = useWorkspace.getState();
      const active = workspace.active ? workspace.tabs[workspace.active] : undefined;
      const schema = sameDb(active?.console.db, db) ? (active?.console.schema ?? null) : null;
      const config = await commands.consoleConfigure(id, { ...db, schema, mode: "AutoCommit" });
      if (!alive.current) {
        await commands.consoleClose(id, true);
        return;
      }
      if (config.status === "error") {
        setError(configureErrorText(config.error));
        return;
      }
      const reply = await commands.consoleRun(id, { kind: "script", text: sql, target: { kind: "all" }, mode: "Run", params: [] }, null);
      if (!alive.current) {
        await commands.consoleClose(id, true);
        return;
      }
      if (reply.status === "error")
        setError(reply.error.kind === "ConfirmWrites" ? "Add this SQL to a notebook cell to review and approve the write." : runErrorText(reply.error));
    } catch (e) {
      if (alive.current) setError(String(e));
    } finally {
      if (alive.current) setStarting(false);
    }
  };
  const state = console?.run;
  const result = state ? rowResults(state)[0] : undefined;
  const now = useNow(state?.kind === "Running", 100);
  const summary = state ? runSummary(state, undefined, now) : null;
  return { run, stop: () => void commands.consoleCancel(id), busy: starting || (console?.work !== undefined && console.work !== null), result, summary, error };
}

export function InlineRows({ columns, rows }: { columns: string[]; rows: (string | null)[][] }) {
  return (
    <div className="max-h-56 overflow-auto">
      <table className="w-full border-collapse text-ui-11 tabular-nums">
        <thead>
          <tr>
            {columns.map((name, i) => (
              <th key={i} className="sticky top-0 border-b border-(--chat-border) bg-canvas px-3 py-1.5 text-left font-normal text-muted">
                {name}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} className="border-b border-(--chat-border)/50 last:border-0">
              {row.map((value, c) => (
                <td key={c} title={value ?? "NULL"} className="max-w-48 truncate px-3 py-1.5 font-mono text-fg">
                  {value ?? <span className="text-dim">NULL</span>}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function InlineQueryRows({ result }: { result: NonNullable<ReturnType<typeof useInlineQuery>["result"]> }) {
  const [page, setPage] = useState<RowPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const meta = result.meta;
  useEffect(() => {
    let current = true;
    void commands
      .resultPage(meta.ref, 0, 6)
      .then((reply) => {
        if (!current) return;
        if (reply.status === "ok") {
          setPage(reply.data);
          setError(null);
        } else setError("Could not load the result preview.");
      })
      .catch(() => {
        if (current) setError("Could not load the result preview.");
      });
    return () => {
      current = false;
    };
  }, [meta.ref, meta.rowCount]);
  return (
    <div data-testid="inline-query-results">
      {error ? (
        <p role="alert" className="px-3 py-2 text-ui-11 text-danger">
          {error}
        </p>
      ) : (
        page && <InlineRows columns={meta.columns.map((c) => c.name)} rows={page.rows} />
      )}
      {meta.rowCount > 6 && (
        <p className="px-3 py-2 text-ui-10 text-dim">Showing the first 6 of {meta.rowCount.toLocaleString()} rows. Add to a cell for the full grid.</p>
      )}
    </div>
  );
}
