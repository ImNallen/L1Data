import { SearchIcon, XIcon } from "lucide-react";
import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Screen } from "../../state/workspace";
import { cx, IconButton } from "../../ui/primitives";

/** The sidebar's filter box. `count` is how many objects the query matches. */
export function SidebarSearch({
  testId,
  scope,
  query,
  count,
  onQueryChange,
  onEnter,
  onArrowDown,
  children,
  placeholder = "Search",
  showCount = true,
}: {
  testId: string;
  scope: Screen["kind"];
  query: string;
  count: number;
  onQueryChange: (query: string) => void;
  onEnter?: () => void;
  onArrowDown?: () => void;
  children?: ReactNode;
  placeholder?: string;
  showCount?: boolean;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [focused, setFocused] = useState(false);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") onEnter?.();
    if (e.key === "ArrowDown" && onArrowDown) {
      e.preventDefault();
      onArrowDown();
    }
    if (e.key !== "Escape" || scope === "settings") return;
    if (query) {
      e.preventDefault();
      onQueryChange("");
    }
    e.currentTarget.blur();
  };

  return (
    <div className="mx-2 flex h-[34px] shrink-0 items-center gap-1">
      <label
        className={cx(
          "group flex h-full min-w-0 flex-1 cursor-text items-center gap-2.5 rounded-md border pr-1.5 pl-2.5 text-muted",
          focused ? "border-focus bg-transparent" : "border-transparent bg-transparent",
        )}
      >
        <SearchIcon size={15} className="shrink-0" />
        <input
          ref={input}
          data-testid={testId}
          aria-label={placeholder}
          type="text"
          value={query}
          placeholder={placeholder}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => onQueryChange(e.target.value)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onKeyDown={onKeyDown}
          className="h-full min-w-0 flex-1 bg-transparent text-ui-12 text-fg outline-none placeholder:text-muted"
        />
        {query && (
          <>
            {showCount && (
              <span data-testid={`${testId}-count`} className="shrink-0 text-ui-11.5 text-muted tabular-nums">
                {count}
              </span>
            )}
            <IconButton
              data-testid={`${testId}-clear`}
              aria-label="Clear search"
              className="size-5"
              onClick={() => {
                onQueryChange("");
                input.current?.focus();
              }}
            >
              <XIcon size={13} />
            </IconButton>
          </>
        )}
      </label>
      {children}
    </div>
  );
}
