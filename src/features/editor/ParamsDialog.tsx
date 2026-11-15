import { useRef, useState } from "react";
import type { ParamValue } from "../../ipc/bindings";
import { Button, Modal } from "../../ui/primitives";
import { finishParams, useParams, type ParamPrompt } from "./paramsState";

function Prompt({ prompt }: { prompt: ParamPrompt }) {
  const [values, setValues] = useState<ParamValue[]>(() => prompt.params.map(({ name }) => ({ name, value: "" })));
  const first = useRef<HTMLInputElement>(null);
  const run = () => finishParams(values);
  return (
    <Modal
      open
      title="Query parameters"
      description={prompt.description}
      width={440}
      testId="params-dialog"
      initialFocus={first}
      onOpenChange={(open) => !open && finishParams(null)}
    >
      <form
        onSubmit={(event) => {
          event.preventDefault();
          run();
        }}
      >
        <div className="mb-5 flex max-h-[42vh] flex-col gap-3 overflow-y-auto pr-1">
          {values.map((param, index) => (
            <div key={param.name} className="flex items-center gap-3">
              <label htmlFor={`param-${index}`} title={param.name} className="w-20 shrink-0 truncate font-mono text-ui-12.5 text-fg">
                {param.name}
              </label>
              <input
                ref={index === 0 ? first : undefined}
                id={`param-${index}`}
                data-testid={`param-value-${param.name}`}
                className="min-w-0 flex-1 rounded-md border border-control bg-bg px-2.5 py-1.5 font-mono text-ui-12.5 text-fg outline-none focus:border-focus disabled:opacity-40"
                value={param.value ?? ""}
                disabled={param.value === null}
                onChange={(event) => setValues(values.map((p, i) => (i === index ? { ...p, value: event.target.value } : p)))}
              />
              <label className="flex items-center gap-1.5 text-ui-12 text-dim">
                <input
                  type="checkbox"
                  data-testid={`param-null-${param.name}`}
                  aria-label={`${param.name} is NULL`}
                  checked={param.value === null}
                  onChange={(event) => setValues(values.map((p, i) => (i === index ? { ...p, value: event.target.checked ? null : "" } : p)))}
                />
                NULL
              </label>
            </div>
          ))}
        </div>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" data-testid="params-cancel" onClick={() => finishParams(null)}>
            Cancel
          </Button>
          <Button type="submit" variant="primary" data-testid="params-run">
            Run
          </Button>
        </div>
      </form>
    </Modal>
  );
}

export function ParamsDialog() {
  const prompt = useParams((state) => state.prompt);
  return prompt && <Prompt key={prompt.sequence} prompt={prompt} />;
}
