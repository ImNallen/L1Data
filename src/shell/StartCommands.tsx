import { shownDb, useWorkspace } from "../state/workspace";
import { Kbd } from "../ui/primitives";
import { commandLabel, shortcutLabel, startSections } from "./commands";

export function StartCommands() {
  const db = useWorkspace(shownDb);
  return (
    <div data-testid="no-tabs" className="flex h-full flex-col overflow-y-auto p-5">
      <div className="mx-auto my-auto w-full max-w-[320px] shrink-0">
        {startSections(db !== null).map((section, s) => (
          <ul key={s} className={s > 0 ? "mt-2 flex flex-col gap-0.5 border-t border-border pt-2" : "flex flex-col gap-0.5"}>
            {section.map((c, i) => (
              <li key={c.id}>
                <button
                  type="button"
                  data-testid={`start-${c.id}`}
                  onClick={c.run}
                  className={`flex min-h-8 w-full items-center justify-between gap-3 rounded-md px-2.5 py-1 text-ui-13.5 hover:bg-hover hover:text-fg focus-visible:outline-2 focus-visible:outline-focus ${s === 0 && i === 0 ? "text-fg" : "text-muted"}`}
                >
                  {commandLabel(c)}
                  {c.key && <Kbd className="shrink-0 text-muted">{shortcutLabel(c)}</Kbd>}
                </button>
              </li>
            ))}
          </ul>
        ))}
      </div>
    </div>
  );
}
