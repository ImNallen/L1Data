import * as Menu from "@radix-ui/react-dropdown-menu";
import { NetworkIcon, PlusIcon, SquareCodeIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { TabId } from "../../ipc/bindings";
import { useCatalog, viewFor } from "../../state/catalog";
import { activeTab, shownDb, useWorkspace } from "../../state/workspace";
import { cx, IconButton, MENU_ITEM, MenuContent, MenuLabel } from "../../ui/primitives";
import { openMapTab, openSqlTab } from "./runActions";

function Item({
  icon,
  children,
  hint,
  testId,
  disabled,
  onSelect,
}: {
  icon: ReactNode;
  children: ReactNode;
  hint?: string;
  testId: string;
  disabled?: boolean;
  onSelect: () => void;
}) {
  return (
    <Menu.Item data-testid={testId} disabled={disabled} onSelect={onSelect} className={cx(MENU_ITEM, "text-fg data-[disabled]:opacity-40")}>
      <span className="grid w-3.5 place-items-center text-muted">{icon}</span>
      {children}
      {hint && <span className="ml-auto pl-6 text-ui-11 text-dim">{hint}</span>}
    </Menu.Item>
  );
}

export function AddCellMenu({ after, testId, className }: { after?: TabId; testId: string; className?: string }) {
  const db = useWorkspace(shownDb);
  const schema = useWorkspace((s) => activeTab(s)?.console.schema);
  const defaultSchema = useCatalog((s) => viewFor(s.views, db)?.catalog.defaultSchema);
  const mapped = schema ?? defaultSchema;
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger asChild>
        <IconButton data-testid={testId} aria-label="Add cell" title="Add cell" className={className}>
          <PlusIcon size={14} />
        </IconButton>
      </Menu.Trigger>
      <MenuContent>
        <MenuLabel>Add cell</MenuLabel>
        <Item testId="add-cell-sql" icon={<SquareCodeIcon size={14} />} onSelect={() => openSqlTab({ after })}>
          SQL query
        </Item>
        <Item
          testId="add-cell-map"
          icon={<NetworkIcon size={14} />}
          hint={db ? [db.database, mapped].filter(Boolean).join(" · ") : undefined}
          disabled={!db}
          onSelect={() => openMapTab({ after })}
        >
          Schema map
        </Item>
      </MenuContent>
    </Menu.Root>
  );
}
