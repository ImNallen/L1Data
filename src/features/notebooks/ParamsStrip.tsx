import { XIcon } from "lucide-react";
import { useId } from "react";
import type { NotebookId, ParamValue } from "../../ipc/bindings";
import { useWorkspace } from "../../state/workspace";
import { IconButton } from "../../ui/primitives";

const NO_PARAMS: ParamValue[] = [];

/** The notebook's values. An edit applies as it is typed, as a cell's text does, so a file notebook reads unsaved at once. */
export function ParamsStrip({ id }: { id: NotebookId }) {
  const params = useWorkspace((s) => s.notebooks[id]?.params ?? NO_PARAMS);
  const prefix = useId();
  if (params.length === 0) return null;
  const set = (name: string, value: string | null) => useWorkspace.getState().mergeParams(id, [{ name, value }]);
  return (
    <section data-testid="notebook-params" aria-label="Parameters" className="notebook-params">
      <span className="text-ui-11 font-medium text-dim">Parameters</span>
      {params.map(({ name, value }, index) => (
        <div key={name} className="flex min-w-0 items-center gap-1.5">
          <label htmlFor={`${prefix}-${index}`} title={name} className="max-w-40 truncate font-mono text-ui-12 text-fg">
            {name}
          </label>
          <input
            id={`${prefix}-${index}`}
            data-testid={`notebook-param-value-${name}`}
            className="w-32 min-w-0 rounded-md border border-control bg-bg px-2 py-0.5 font-mono text-ui-12 text-fg outline-none focus:border-focus disabled:opacity-40"
            value={value ?? ""}
            placeholder={value === null ? "NULL" : undefined}
            disabled={value === null}
            onChange={(event) => set(name, event.target.value)}
          />
          <label className="flex items-center gap-1 text-ui-11 text-dim">
            <input
              type="checkbox"
              data-testid={`notebook-param-null-${name}`}
              aria-label={`${name} is NULL`}
              checked={value === null}
              onChange={(event) => set(name, event.target.checked ? null : "")}
            />
            NULL
          </label>
          <IconButton
            className="size-6"
            data-testid={`notebook-param-remove-${name}`}
            aria-label={`Remove ${name}`}
            title={`Remove ${name}`}
            onClick={() => useWorkspace.getState().removeParam(id, name)}
          >
            <XIcon size={12} />
          </IconButton>
        </div>
      ))}
    </section>
  );
}
