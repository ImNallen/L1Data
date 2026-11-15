import { create } from "zustand";
import type { NotebookId, Param, ParamValue } from "../../ipc/bindings";
import { missingParams } from "../../state/notebookParams";
import { useWorkspace } from "../../state/workspace";

let sequence = 0;

export type ParamPrompt = {
  sequence: number;
  params: Param[];
  description: string;
  valid: () => boolean;
  resolve: (values: ParamValue[] | null) => void;
};

export const useParams = create<{ prompt: ParamPrompt | null }>(() => ({ prompt: null }));

export function finishParams(values: ParamValue[] | null): void {
  const { prompt } = useParams.getState();
  if (!prompt) return;
  useParams.setState({ prompt: null });
  prompt.resolve(prompt.valid() ? values : null);
}

export function cancelStaleParams(): void {
  const { prompt } = useParams.getState();
  if (prompt && !prompt.valid()) finishParams(null);
}

/**
 * The notebook's whole list once it has a value for each of `names`. It asks only for the names it has no value for,
 * each starting empty, and merges the accepted values into the notebook. `null` on Cancel, or once `valid` fails.
 */
export async function resolveParams(id: NotebookId, names: readonly Param[], description: string, valid: () => boolean): Promise<ParamValue[] | null> {
  const missing = missingParams(names, useWorkspace.getState().notebooks[id]?.params ?? []);
  if (missing.length > 0) {
    finishParams(null);
    const accepted = await new Promise<ParamValue[] | null>((resolve) =>
      useParams.setState({ prompt: { sequence: ++sequence, params: missing, description, valid, resolve } }),
    );
    if (!accepted || !valid()) return null;
    useWorkspace.getState().mergeParams(id, accepted);
  }
  return useWorkspace.getState().notebooks[id]?.params ?? null;
}
