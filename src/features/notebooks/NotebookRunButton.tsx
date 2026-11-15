import { PlayIcon, SquareIcon } from "lucide-react";
import type { Environment, NotebookId } from "../../ipc/bindings";
import { Button } from "../../ui/primitives";
import { runNotebook, stopNotebookRun, useNotebookRun } from "./notebookRun";
import { progressText } from "./runbook";

export function NotebookRunButton({ id, environment }: { id: NotebookId; environment: Environment | null }) {
  const run = useNotebookRun(id);
  if (!run)
    return (
      <Button
        data-testid="run-all-btn"
        data-environment={environment ?? undefined}
        variant="primary"
        title="Run every cell in order, and stop at the first that fails"
        className="run-environment @max-[520px]:px-2.5"
        onClick={() => void runNotebook(id, "all")}
      >
        <PlayIcon fill="currentColor" size={12} className="shrink-0 text-run-icon" />
        <span className="@max-[520px]:sr-only">Run all</span>
      </Button>
    );
  const progress = progressText(run);
  return (
    <Button
      data-testid="stop-run-btn"
      title="Cancel the running cell and skip the rest"
      disabled={run.phase === "stopping"}
      className="bg-hover tabular-nums"
      onClick={() => stopNotebookRun(id)}
    >
      <SquareIcon fill="currentColor" size={10} className="shrink-0 text-muted" />
      {run.phase === "stopping" ? "Stopping…" : "Stop"}
      {progress && (
        <span data-testid="run-progress" role="status" className="text-dim">
          {progress}
        </span>
      )}
    </Button>
  );
}
