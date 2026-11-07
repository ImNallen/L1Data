import * as Menu from "@radix-ui/react-dropdown-menu";
import * as HoverCard from "@radix-ui/react-hover-card";
import { DownloadIcon, RefreshCwIcon, RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { useLive } from "../../state/live";
import { cx, IconButton, MENU_ITEM, Spinner } from "../../ui/primitives";
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

function ChangesCard({ lead, sections }: { lead: string; sections: ChangeSection[] }) {
  return (
    <HoverCard.Portal>
      <HoverCard.Content
        data-testid="update-changes"
        side="top"
        align="end"
        sideOffset={6}
        collisionPadding={8}
        className="motion-pop z-50 flex max-h-[min(420px,70vh)] w-80 flex-col overflow-hidden rounded-lg border border-control bg-raised text-ui-12.5 shadow-[var(--shadow-popover)]"
      >
        <p className="shrink-0 px-3 py-2.5 text-fg">{lead}</p>
        <div className="min-h-0 overflow-y-auto border-t border-control px-3 pt-1 pb-2.5">
          {sections.map((section) => (
            <section key={section.heading} className="pt-2">
              <h3 className="pb-1 font-medium text-fg">{section.heading}</h3>
              <ul className="list-disc space-y-1 pl-4 marker:text-dim">
                {section.items.map((item, i) => (
                  <li key={i} className="text-fg">
                    {item.title}
                    {item.credit && <span className="text-muted"> {item.credit}</span>}
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      </HoverCard.Content>
    </HoverCard.Portal>
  );
}

/** Not `disabled` while a step runs, because a disabled button shows no tooltip with the progress. */
export function UpdateButton({ menu = false }: { menu?: boolean }) {
  const updates = useLive((s) => s.updates);
  const now = useNow(updates?.status.kind === "UpToDate", 60_000);
  const view = updates && updateButtonView(updates, now);
  if (!updates || !view) return null;
  const { action } = view;
  const sections = changeSections(updates.status);
  const card = sections.length > 0;
  // The root stays mounted as the status moves, so the card stays open across the click that starts the download.
  return (
    <HoverCard.Root openDelay={400} closeDelay={200}>
      <HoverCard.Trigger asChild>
        {menu ? (
          <Menu.Item
            data-testid="update-button"
            data-status={updates.status.kind}
            disabled={action === null}
            onSelect={() => action && runUpdateAction(action)}
            className={cx(MENU_ITEM, "text-fg data-disabled:opacity-50")}
          >
            <Glyph view={view} />
            <span>{view.label}</span>
          </Menu.Item>
        ) : (
          <IconButton
            data-testid="update-button"
            data-status={updates.status.kind}
            aria-label={view.label}
            aria-disabled={action === null}
            title={card ? undefined : view.tooltip}
            className="size-[34px] shrink-0 relative ml-auto rounded-full aria-disabled:cursor-default aria-disabled:hover:bg-transparent"
            onClick={() => action && runUpdateAction(action)}
          >
            <Glyph view={view} />
            {view.attention && <span className="absolute top-0.5 right-0.5 size-1.5 rounded-full bg-run" />}
          </IconButton>
        )}
      </HoverCard.Trigger>
      {card && <ChangesCard lead={view.tooltip} sections={sections} />}
    </HoverCard.Root>
  );
}
