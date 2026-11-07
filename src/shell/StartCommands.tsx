import { Kbd } from "../ui/primitives";
import { START_COMMANDS, shortcutLabel } from "./commands";

export function StartCommands() {
  return (
    <div data-testid="no-tabs" className="flex h-full flex-col overflow-y-auto bg-bg p-5">
      <div className="mx-auto my-auto w-full max-w-[320px] shrink-0">
        <div className="mb-3 px-2.5">
          <h1 className="text-ui-14 font-semibold text-fg">Your SQL workspace</h1>
          <p className="mt-1 text-ui-13 text-dim">Open a query or connect to a database to begin.</p>
        </div>
        <ul className="flex flex-col gap-0.5">
          {START_COMMANDS.map((c, i) => (
            <li key={c.id} className={i === 3 ? "mt-2 border-t border-border pt-2" : undefined}>
              <button
                type="button"
                data-testid={`start-${c.id}`}
                onClick={c.run}
                className={`flex min-h-8 w-full items-center justify-between gap-3 rounded-md px-2.5 py-1 text-ui-13.5 hover:bg-hover hover:text-fg focus-visible:outline-2 focus-visible:outline-focus ${i === 0 ? "text-fg" : "text-muted"}`}
              >
                {c.label}
                <Kbd className="shrink-0 text-muted">{shortcutLabel(c)}</Kbd>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
