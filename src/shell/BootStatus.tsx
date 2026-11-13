import { Button } from "../ui/primitives";

type StartupState = { kind: "starting" } | { kind: "failed"; error: string; bridge: boolean; retry: () => void };

export function BootStatus({ state }: { state: StartupState }) {
  return (
    <main data-testid="boot-status" className="grid h-full place-items-center text-ui-13.5 text-muted">
      {state.kind === "starting" ? (
        <div className="relative h-[360px] max-h-full w-full max-w-[560px]">
          <div className="absolute inset-x-0 top-[102px] flex flex-col items-center gap-[20px] text-fg">
            <svg width="68" height="59.5" viewBox="32 32 320 280" fill="currentColor" aria-hidden="true" focusable="false">
              <g transform="translate(32 32)">
                <path d="M32 0H70Q92 0 92 22V162Q92 191 119 195L191 201Q202 202 202 214V267Q202 280 189 280H35Q0 280 0 245V34Q0 0 32 0Z" />
                <path d="M159 53L237 6Q247 0 259 0H296Q320 0 320 24V256Q320 280 296 280H246Q224 280 224 258V99Q224 93 218 97L166 127Q150 136 150 118V70Q150 59 159 53Z" />
              </g>
            </svg>
            <h1 className="text-[24px] leading-[29px] font-semibold">L1 Data</h1>
          </div>
          <p role="status" className="absolute inset-x-0 top-[282px] flex items-center justify-center gap-2 text-[11px] leading-4">
            <span aria-hidden="true" className="boot-activity flex gap-[3px] text-keyword">
              <span />
              <span />
              <span />
            </span>
            Starting L1 Data
          </p>
        </div>
      ) : (
        <div className="max-w-lg space-y-3 px-6">
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
