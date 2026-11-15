import type { Param, ParamValue } from "../ipc/bindings";

/** Equal keys mean equal lists, order included. */
export function paramsKey(params: readonly ParamValue[]): string {
  return JSON.stringify(params.map((p) => [p.name, p.value]));
}

/** `params` with each of `values` set by name. A known name keeps its place, and a new one goes last. The same array when nothing changes. */
export function mergedParams(params: ParamValue[], values: readonly ParamValue[]): ParamValue[] {
  let merged = params;
  for (const value of values) {
    const at = merged.findIndex((p) => p.name === value.name);
    if (at === -1) merged = [...merged, value];
    else if (merged[at]?.value !== value.value) merged = merged.with(at, value);
  }
  return merged;
}

/** The same array when `name` has no value. */
export function withoutParam(params: ParamValue[], name: string): ParamValue[] {
  return params.some((p) => p.name === name) ? params.filter((p) => p.name !== name) : params;
}

/** The names in `found` that `params` has no value for, each once, in the order given. NULL is a value. */
export function missingParams(found: readonly Param[], params: readonly ParamValue[]): Param[] {
  const missing: Param[] = [];
  for (const p of found) if (!params.some((v) => v.name === p.name) && !missing.some((m) => m.name === p.name)) missing.push(p);
  return missing;
}
