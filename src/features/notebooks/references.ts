/** A cell a run inlined through `{{name}}`, with the text it used. */
export type Source = { name: string; text: string };

export type Staleness = { name: string; why: "changed" | "missing" | "ambiguous" };

/**
 * Why a cell's result no longer matches its references. A run returns every cell it inlined, transitively, so a change
 * two references away shows here too. Re-running a referenced cell alone changes nothing: the referencing cell runs
 * that cell's SQL itself.
 */
export function staleness(sources: readonly Source[], cells: readonly Source[]): Staleness[] {
  return sources.flatMap((source): Staleness[] => {
    const named = cells.filter((c) => c.name === source.name);
    if (named.length === 0) return [{ name: source.name, why: "missing" }];
    if (named.length > 1) return [{ name: source.name, why: "ambiguous" }];
    return named[0]?.text === source.text ? [] : [{ name: source.name, why: "changed" }];
  });
}

export function stalenessText(reasons: readonly Staleness[]): string {
  return reasons
    .map(({ name, why }) =>
      why === "changed" ? `${name} changed since this cell ran.` : why === "missing" ? `No cell is named ${name} now.` : `Several cells are named ${name} now.`,
    )
    .join(" ");
}
