import { useEffect, useId, useState } from "react";

/**
 * A palette's selected option, held by key so it stays on the same item while the list filters.
 * `fallback` is the index selected when no item has the held key.
 */
export function useListSelection<T>(items: readonly T[], keyOf: (item: T) => string, fallback = 0) {
  const [selectedKey, select] = useState<string | null>(null);
  const listId = useId();
  const found = items.findIndex((item) => keyOf(item) === selectedKey);
  const selectedIndex = found >= 0 ? found : fallback;
  const selected: T | undefined = items[selectedIndex];
  const optionId = (index: number) => `${listId}-${index}`;
  const selectedOption = optionId(selectedIndex);
  const shownKey = selected === undefined ? undefined : keyOf(selected);
  useEffect(() => {
    document.getElementById(selectedOption)?.scrollIntoView({ block: "nearest" });
  }, [selectedOption, shownKey]);
  const move = (step: 1 | -1) => {
    const next = items[(selectedIndex + step + items.length) % items.length];
    if (next !== undefined) select(keyOf(next));
  };
  return { listId, optionId, selected, selectedIndex, select, move };
}
