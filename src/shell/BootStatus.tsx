import { Button, Spinner } from "../ui/primitives";

type StartupState = { kind: "starting" } | { kind: "failed"; error: string; bridge: boolean; retry: () => void };

export function BootStatus({ state }: { state: StartupState }) {
  return (
    <main data-testid="boot-status" className="grid h-full place-items-center px-6 text-ui-13.5 text-muted">
      {state.kind === "starting" ? (
        <p role="status" className="flex items-center gap-2">
          <Spinner /> Starting L1 Data…
        </p>
      ) : (
        <div className="max-w-lg space-y-3">
          <h1 className="text-ui-16 font-semibold text-fg">Could not start L1 Data</h1>
          {state.bridge && <p>The engine may still be starting. Wait a moment, then retry.</p>}
          <p role="alert" className="selectable break-words text-dim">
            {state.error}
          </p>
          <Button variant="primary" onClick={state.retry}>
            Retry
          </Button>
        </div>
      )}
    </main>
  );
}
