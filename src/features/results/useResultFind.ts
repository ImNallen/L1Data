import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { commands, type ResultCell, type ResultFind, type ResultFindDirection, type ResultRef } from "../../ipc/bindings";

export type FindState = { kind: "closed" } | { kind: "searching" } | { kind: "ready"; result: ResultFind } | { kind: "failed"; message: string };

export function useResultFind(resultRef: ResultRef, rowCount: number, onTarget: (cell: ResultCell) => void) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [state, setState] = useState<FindState>({ kind: "closed" });
  const [direction, setDirection] = useState<ResultFindDirection>({ kind: "First" });
  const generation = useRef(0);
  const inFlight = useRef<Promise<void> | null>(null);
  const target = useRef(onTarget);
  useLayoutEffect(() => {
    target.current = onTarget;
  }, [onTarget]);
  const { tabId, runId, index } = resultRef;
  useLayoutEffect(() => {
    const invalidate = () => {
      ++generation.current;
    };
    invalidate();
    return invalidate;
  }, [tabId, runId, index]);
  useEffect(() => {
    const ticket = ++generation.current;
    let cancelled = false;
    const current = () => !cancelled && ticket === generation.current;
    if (!open) return;
    const timer = setTimeout(() => {
      setState({ kind: "searching" });
      const run = async () => {
        await inFlight.current;
        if (!current()) return;
        const work = async () => {
          try {
            const reply = await commands.resultFind({ tabId, runId, index }, query, direction);
            if (!current()) return;
            if (reply.status === "error") {
              setState({ kind: "failed", message: reply.error.kind === "Gone" ? "This result has been replaced." : "Could not search this result." });
              return;
            }
            const result = reply.data;
            setState({ kind: "ready", result });
            if (result.target) target.current(result.target.cell);
          } catch {
            if (!current()) return;
            setState({ kind: "failed", message: "Could not search this result. It may have been replaced." });
          }
        };
        inFlight.current = work();
        await inFlight.current;
        if (!current()) return;
      };
      void run();
    }, 200);
    return () => {
      clearTimeout(timer);
      cancelled = true;
    };
  }, [open, query, tabId, runId, index, direction]);
  useEffect(() => {
    if (!open || state.kind !== "ready" || state.result.rowCount === rowCount) return;
    const timer = setTimeout(() => setDirection({ ...direction }), 200);
    return () => clearTimeout(timer);
  }, [open, state, rowCount, direction]);
  return {
    state,
    query,
    open,
    show: () => {
      setOpen(true);
    },
    close: () => {
      ++generation.current;
      setOpen(false);
      setState({ kind: "closed" });
    },
    change: (value: string) => {
      ++generation.current;
      setState({ kind: "searching" });
      setQuery(value);
      setDirection({ kind: "First" });
    },
    navigate: (back: boolean) => {
      if (state.kind !== "ready" || !state.result.target) return;
      const anchor = state.result.target.cell;
      setState({ kind: "searching" });
      ++generation.current;
      setDirection({ kind: back ? "Previous" : "Next", anchor });
    },
  };
}

export function storedValueMatches(value: string | null | undefined, query: string): boolean {
  return (
    !!query &&
    typeof value === "string" &&
    Array.from(value, (c) => c.toLowerCase())
      .join("")
      .includes(Array.from(query, (c) => c.toLowerCase()).join(""))
  );
}

export function useCutCellMatch(ref: ResultRef, row: number, col: number, query: string) {
  const [match, setMatch] = useState<{ key: string; value: boolean } | null>(null);
  const { tabId, runId, index } = ref;
  const key = JSON.stringify([tabId, runId, index, row, col, query]);
  useEffect(() => {
    if (!query) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      void commands.resultCellMatches({ tabId, runId, index }, { row, col }, query).then(
        (reply) => {
          if (!cancelled && reply.status === "ok") setMatch({ key, value: reply.data });
        },
        () => {},
      );
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [tabId, runId, index, row, col, query, key]);
  return match?.key === key && match.value;
}
