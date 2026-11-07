import { Separator } from "react-resizable-panels";
import { cx } from "../ui/primitives";

// The line runs vertically between side-by-side panels and horizontally between stacked ones. A keyboard focus turns the
// line the focus colour, the only sign of focus the handle has. The library's own `focus` state also follows a click, so
// it is not used.
const LINE = {
  vertical: "w-px after:inset-y-0 after:-inset-x-1",
  horizontal: "h-px after:inset-x-0 after:-inset-y-1",
} as const;

export const handleLook = (line: keyof typeof LINE, lineColor: string) =>
  cx(
    "relative z-10 outline-none transition-colors after:absolute hover:bg-handle-hover data-[separator=hover]:bg-handle-hover focus-visible:bg-focus data-[separator=active]:bg-run",
    LINE[line],
    lineColor,
  );

export function ResizeHandle({ id, label, line, lineColor }: { id: string; label: string; line: keyof typeof LINE; lineColor: string }) {
  return <Separator id={id} aria-label={label} className={handleLook(line, lineColor)} />;
}
