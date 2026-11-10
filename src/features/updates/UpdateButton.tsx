import * as HoverCard from "@radix-ui/react-hover-card";
import { DownloadIcon, RefreshCwIcon, RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { useLive } from "../../state/live";
import { cx, IconButton, Spinner } from "../../ui/primitives";
import { useNow } from "../../ui/useNow";
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

function UpdateCard({ view, sections }: { view: UpdateButtonView; sections: ChangeSection[] }) {
  return (
    <HoverCard.Portal>
      <HoverCard.Content
        data-testid="update-hover"
        side="top"
        align="end"
        sideOffset={6}
        collisionPadding={8}
        className="motion-pop z-50 flex max-h-[min(420px,70vh)] w-60 max-w-[calc(100vw-16px)] flex-col gap-2.5 overflow-hidden rounded-lg border border-border bg-raised p-3.5 text-ui-12 leading-[1.5] text-fg"
      >
        <p className="shrink-0 text-ui-13 font-semibold break-words">{view.heading}</p>
        <p className="shrink-0 text-ui-11 break-words text-muted">{view.tooltip}</p>
        {sections.length > 0 && <div className="h-px shrink-0 bg-border" />}
        {sections.length > 0 && (
          <div data-testid="update-changes" className="min-h-0 space-y-2.5 overflow-y-auto">
            {sections.map((section) => (
              <section key={section.heading}>
                <h3 className="pb-2.5 font-medium">{section.heading}</h3>
                <ul className="list-disc space-y-2.5 pl-3">
                  {section.items.map((item, i) => (
                    <li key={i}>
                      <p className="break-words">{item.title}</p>
                      {item.credit && <p className="mt-0.5 -ml-3 text-ui-11 break-words text-muted">{item.credit}</p>}
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
        {sections.length > 0 && <p className="shrink-0 text-ui-10 text-dim">GitHub-generated release notes</p>}
      </HoverCard.Content>
    </HoverCard.Portal>
  );
}

/** Keep the button focusable while a step runs so its hover card can explain the progress. */
export function UpdateButton() {
  const updates = useLive((s) => s.updates);
  const now = useNow(updates?.status.kind === "UpToDate", 60_000);
  const view = updates && updateButtonView(updates, now);
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
      <UpdateCard view={view} sections={sections} />
    </HoverCard.Root>
  );
}
