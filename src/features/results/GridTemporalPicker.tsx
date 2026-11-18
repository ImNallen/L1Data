import { ChevronLeftIcon, ChevronRightIcon } from "lucide-react";
import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { cx } from "../../ui/primitives";
import type { TemporalShape } from "./gridEditorType";
import {
  dayText,
  daysInMonth,
  freshTemporal,
  localDay,
  mondayIndex,
  moveDay,
  moveMonth,
  parseDay,
  temporalText,
  type CalendarDay,
  type TemporalParts,
} from "./gridEditorValue";

export const POPUP_FIELD = "h-8 min-w-0 rounded-lg border border-control bg-bg px-2 font-mono text-ui-12 text-fg outline-none focus:border-focus";
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

export function GridTemporalPicker({
  parts,
  shape,
  onChange,
  onExact,
  error,
  actions,
}: {
  parts: TemporalParts;
  shape: TemporalShape;
  onChange(parts: TemporalParts): void;
  onExact(): void;
  error: string | null;
  actions: ReactNode;
}) {
  const selected = parseDay(parts.date);
  const [focusDay, setFocusDay] = useState<CalendarDay>(() => selected ?? localDay());
  const [jump, setJump] = useState(false);
  const calendar = useRef<HTMLDivElement>(null);
  const first = { ...focusDay, day: 1 };
  const start = mondayIndex(first);
  const count = Math.ceil((start + daysInMonth(first.year, first.month)) / 7) * 7;
  const navigate = (day: CalendarDay) => {
    setFocusDay(day);
    requestAnimationFrame(() => calendar.current?.querySelector<HTMLButtonElement>(`[data-day="${dayText(day)}"]`)?.focus());
  };
  const calendarKey = (event: KeyboardEvent, day: CalendarDay) => {
    let next: CalendarDay;
    switch (event.key) {
      case "ArrowLeft":
        next = moveDay(day, -1);
        break;
      case "ArrowRight":
        next = moveDay(day, 1);
        break;
      case "ArrowUp":
        next = moveDay(day, -7);
        break;
      case "ArrowDown":
        next = moveDay(day, 7);
        break;
      case "PageUp":
        next = moveMonth(day, event.shiftKey ? -12 : -1);
        break;
      case "PageDown":
        next = moveMonth(day, event.shiftKey ? 12 : 1);
        break;
      case "Home":
        next = moveDay(day, -mondayIndex(day));
        break;
      case "End":
        next = moveDay(day, 6 - mondayIndex(day));
        break;
      default:
        return;
    }
    event.preventDefault();
    navigate(next);
  };
  return (
    <>
      {shape.kind !== "time" && (
        <>
          <div className="mb-1 flex h-7 items-center justify-between gap-2">
            {jump ? (
              <div className="flex min-w-0 gap-2">
                <select
                  aria-label="Month"
                  className={POPUP_FIELD}
                  value={focusDay.month}
                  onChange={(e) => setFocusDay(moveMonth(focusDay, Number(e.target.value) - focusDay.month))}
                >
                  {MONTHS.map((name, i) => (
                    <option key={name} value={i + 1}>
                      {name}
                    </option>
                  ))}
                </select>
                <input
                  aria-label="Year"
                  className={cx(POPUP_FIELD, "w-20")}
                  inputMode="numeric"
                  defaultValue={focusDay.year}
                  key={focusDay.year}
                  onBlur={(e) => {
                    if (/^\d{1,4}$/.test(e.target.value) && Number(e.target.value) >= 1)
                      setFocusDay(moveMonth(focusDay, (Number(e.target.value) - focusDay.year) * 12));
                  }}
                />
                <button type="button" aria-label="Close month and year" onClick={() => setJump(false)}>
                  Done
                </button>
              </div>
            ) : (
              <button type="button" aria-label="Choose month and year" className="text-ui-13 text-fg" onClick={() => setJump(true)}>
                {MONTHS[focusDay.month - 1]} {focusDay.year}
              </button>
            )}
            <div className="flex gap-3">
              <button type="button" aria-label="Previous month" onClick={() => setFocusDay(moveMonth(focusDay, -1))}>
                <ChevronLeftIcon size={16} />
              </button>
              <button type="button" aria-label="Next month" onClick={() => setFocusDay(moveMonth(focusDay, 1))}>
                <ChevronRightIcon size={16} />
              </button>
            </div>
          </div>
          <div ref={calendar} role="grid" aria-label="Calendar" className="grid grid-cols-7 gap-y-1 text-center text-ui-12">
            <div role="row" className="contents">
              {["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].map((name) => (
                <span key={name} role="columnheader" className="py-1 text-ui-11 text-dim">
                  {name}
                </span>
              ))}
            </div>
            {Array.from({ length: count / 7 }, (_, week) => (
              <div role="row" className="contents" key={week}>
                {Array.from({ length: 7 }, (_, weekday) => {
                  const index = week * 7 + weekday;
                  if (first.year === 9999 && first.month === 12 && index - start >= 31) return <span key={weekday} role="gridcell" aria-disabled="true" />;
                  const day = moveDay(first, index - start);
                  const value = dayText(day);
                  return (
                    <button
                      type="button"
                      role="gridcell"
                      key={index}
                      data-day={value}
                      aria-label={value}
                      aria-selected={value === parts.date}
                      tabIndex={value === dayText(focusDay) ? 0 : -1}
                      onKeyDown={(e) => calendarKey(e, day)}
                      onClick={() => {
                        setFocusDay(day);
                        onChange({ ...parts, date: value });
                      }}
                      className={cx(
                        "mx-auto h-7 w-10 rounded-lg outline-none focus:ring-1 focus:ring-focus",
                        day.month !== focusDay.month && "text-dim",
                        value === parts.date && "bg-accent/15 text-accent",
                        value !== parts.date && "hover:bg-hover",
                      )}
                    >
                      {day.day}
                    </button>
                  );
                })}
              </div>
            ))}
          </div>
        </>
      )}
      {shape.kind !== "date" && (
        <div
          className={cx(
            "grid gap-2",
            shape.zone === "offset" ? "grid-cols-[1.4fr_1fr_1fr]" : "grid-cols-[1.4fr_1fr]",
            shape.kind !== "time" && "mt-3 border-t border-grid pt-3",
          )}
        >
          <label className="flex min-w-0 flex-col gap-2">
            Time{" "}
            <input
              aria-label="Time"
              placeholder="HH:MM:SS"
              className={POPUP_FIELD}
              value={parts.time}
              onChange={(e) => onChange({ ...parts, time: e.target.value })}
            />
          </label>
          <label className="flex min-w-0 flex-col gap-2">
            Fraction{" "}
            <input
              aria-label="Fraction"
              inputMode="numeric"
              placeholder="000000"
              className={POPUP_FIELD}
              value={parts.fraction}
              onChange={(e) => onChange({ ...parts, fraction: e.target.value })}
            />
          </label>
          {shape.zone === "offset" && (
            <label className="flex min-w-0 flex-col gap-2">
              UTC offset{" "}
              <input
                aria-label="UTC offset"
                placeholder="+01:00"
                className={POPUP_FIELD}
                value={parts.offset}
                onChange={(e) => onChange({ ...parts, offset: e.target.value })}
              />
            </label>
          )}
        </div>
      )}
      <div className="mt-3 flex items-center justify-between gap-2 text-ui-11">
        <span className="text-dim">
          {shape.kind === "date" ? "Date only" : shape.zone === "offset" ? `UTC${parts.offset === "Z" ? "+00:00" : parts.offset}` : "Without time zone"}
        </span>
        <button type="button" className="text-accent" onClick={onExact}>
          Edit exact value
        </button>
      </div>
      {error && (
        <p role="alert" className="mt-3 text-ui-11 text-danger">
          {error}
        </p>
      )}
      <div className="mt-3 flex items-center gap-4 border-t border-grid pt-3 text-muted">
        {shape.kind !== "time" && (
          <button
            type="button"
            onClick={() => {
              const day = localDay();
              setFocusDay(day);
              onChange({ ...parts, date: dayText(day) });
            }}
          >
            Today
          </button>
        )}
        {shape.kind !== "date" && (
          <button
            type="button"
            onClick={() => {
              const fresh = freshTemporal(shape);
              onChange({ ...fresh, separator: parts.separator, fraction: parts.fraction ? "0".repeat(parts.fraction.length) : "" });
              if (shape.kind !== "time") setFocusDay(localDay());
            }}
          >
            Now
          </button>
        )}
        <span className="flex-1" />
        {actions}
        <output aria-label="Exact temporal value" className="sr-only">
          {temporalText(parts)}
        </output>
      </div>
    </>
  );
}
