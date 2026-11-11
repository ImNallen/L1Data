import * as HoverCard from "@radix-ui/react-hover-card";
import { DownloadIcon, RefreshCwIcon, RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { useLive } from "../../state/live";
import { cx, IconButton, Spinner } from "../../ui/primitives";
import { runUpdateAction } from "./updateActions";
import { changeSections, updateButtonView, type ChangeSection, type UpdateButtonView } from "./updateText";

const RING_RADIUS = 6;
const RING_LENGTH = 2 * Math.PI * RING_RADIUS;

function ProgressRing({ percent }: { percent: number | null }) {
  return (
    <svg viewBox="0 0 16 16" className={cx("size-4 -rotate-90", percent === null && "animate-spin")} aria-hidden>
      <circle cx="8" cy="8" r={RING_RADIUS} fill="none" strokeWidth="2" className="stroke-control" />
      <circle
        cx="8"
        cy="8"
        r={RING_RADIUS}
        fill="none"
        strokeWidth="2"
        strokeLinecap="round"
        className="stroke-run transition-[stroke-dashoffset] duration-300"
        strokeDasharray={RING_LENGTH}
        strokeDashoffset={RING_LENGTH * (1 - (percent ?? 25) / 100)}
      />
    </svg>
  );
}

function Glyph({ view }: { view: UpdateButtonView }) {
  switch (view.glyph) {
    case "check":
      return <RefreshCwIcon size={16} />;
    case "spinner":
      return <Spinner />;
    case "available":
      return <DownloadIcon size={16} />;
    case "ring":
      return <ProgressRing percent={view.percent} />;
    case "restart":
      return <RotateCwIcon size={16} />;
    case "error":
      return <TriangleAlertIcon size={16} className="text-amber" />;
    default: {
      const unreachable: never = view.glyph;
      return unreachable;
    }
  }
}

function UpdateHover({ view, sections }: { view: UpdateButtonView; sections: ChangeSection[] }) {
  const card = sections.length > 0;
  return (
    <HoverCard.Portal>
      <HoverCard.Content
        data-testid="update-hover"
        side="top"
        align="end"
        sideOffset={6}
        collisionPadding={8}
        className={cx(
          "motion-pop z-50 border border-border bg-raised text-fg shadow-[var(--shadow-float)]",
          card
            ? "flex max-h-[min(420px,70vh)] w-[312px] max-w-[calc(100vw-16px)] flex-col gap-3 overflow-hidden rounded-lg p-3"
            : "inline-flex max-w-[280px] gap-1.5 rounded-md px-2 py-[5px] text-ui-12",
        )}
      >
        {card ? (
          <>
            <div className="flex shrink-0 items-baseline justify-between gap-2">
              <p className="min-w-0 text-ui-12 font-medium break-words">{view.title}</p>
              {view.detail && <p className="shrink-0 text-ui-11 text-dim">{view.detail}</p>}
            </div>
            <div data-testid="update-changes" className="flex min-h-0 flex-col gap-3.5 overflow-y-auto">
              {sections.map((section) => (
                <section key={section.version ?? ""} className="flex flex-col gap-1.5">
                  {section.version && <h3 className="text-ui-11 font-medium text-dim">{section.version}</h3>}
                  <ul className="flex flex-col gap-1.5">
                    {section.items.map((item, i) => (
                      <li key={i} className="flex gap-3">
                        <span className="min-w-0 flex-1 text-ui-12 break-words">{item.title}</span>
                        {item.pr && <span className="shrink-0 text-ui-11 text-dim tabular-nums">{item.pr}</span>}
                      </li>
                    ))}
                  </ul>
                </section>
              ))}
            </div>
          </>
        ) : (
          <>
            <span className="min-w-0 break-words">{view.title}</span>
            {view.detail && <span className="shrink-0 text-dim">{view.detail}</span>}
          </>
        )}
      </HoverCard.Content>
    </HoverCard.Portal>
  );
}

/** Keep the button focusable while a step runs so its hover card can explain the progress. */
export function UpdateButton() {
  const updates = useLive((s) => s.updates);
  const view = updates && updateButtonView(updates.status);
  if (!updates || !view) return null;
  const { action } = view;
  const sections = changeSections(updates.status);
  // The root stays mounted as the status moves, so the card stays open across the click that starts the download.
  return (
    <HoverCard.Root openDelay={400} closeDelay={200}>
      <HoverCard.Trigger asChild>
        <IconButton
          data-testid="update-button"
          data-status={updates.status.kind}
          aria-label={view.label}
          aria-disabled={action === null}
          className="relative shrink-0 aria-disabled:cursor-default aria-disabled:hover:bg-transparent"
          onClick={() => action && runUpdateAction(action)}
        >
          <Glyph view={view} />
          {view.attention && <span className="absolute top-0.5 right-0.5 size-[5px] rounded-full bg-run" />}
        </IconButton>
      </HoverCard.Trigger>
      <UpdateHover view={view} sections={sections} />
    </HoverCard.Root>
  );
}
