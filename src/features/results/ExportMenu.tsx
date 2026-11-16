import * as Menu from "@radix-ui/react-dropdown-menu";
import { save } from "@tauri-apps/plugin-dialog";
import { DownloadIcon } from "lucide-react";
import { Fragment } from "react";
import { commands, type ExportFormat, type ExportScope, type ExportSummary, type ResultMeta } from "../../ipc/bindings";
import { exportErrorText } from "../../ipc/errors";
import { useLive } from "../../state/live";
import type { RowResult } from "../../state/status";
import { confirm, isTableTab, useWorkspace, type Tab } from "../../state/workspace";
import { IconButton, MenuContent, MenuItem, MenuLabel, Spinner } from "../../ui/primitives";
import { EXPORT_FORMATS, exportMenu, FORMAT_ORDER, type TablePage } from "./exportChoices";
import { pluralRows } from "./format";

export function ExportMenu({
  tab,
  result,
  table,
  disabled,
}: {
  tab: Tab | undefined;
  result: RowResult | undefined;
  table: TablePage | null;
  disabled: boolean;
}) {
  const exporting = useLive((s) => (tab ? s.consoles[tab.id]?.export != null : false));
  const model = result ? exportMenu(result.meta, result.state, table) : null;
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger asChild>
        <IconButton data-testid="export-btn" aria-label="Export" title="Export" disabled={disabled || !model || !tab}>
          {exporting ? <Spinner /> : <DownloadIcon size={16} />}
        </IconButton>
      </Menu.Trigger>
      {tab && result && model && (
        <MenuContent align="end">
          {model.groups.map((group) => (
            <Fragment key={group.scope}>
              <MenuLabel>{group.label}</MenuLabel>
              {FORMAT_ORDER.map((format) => (
                <MenuItem key={format} testId={`export-${group.scope}-${format}`} onSelect={() => void exportResult(tab, result.meta, format, group.scope)}>
                  {EXPORT_FORMATS[format].label}…
                </MenuItem>
              ))}
            </Fragment>
          ))}
          {model.note && <MenuLabel>{model.note}</MenuLabel>}
        </MenuContent>
      )}
    </Menu.Root>
  );
}

async function exportResult(tab: Tab, meta: ResultMeta, format: ExportFormat, scope: ExportScope): Promise<void> {
  const notify = useWorkspace.getState().notify;
  if (scope === "All" && meta.allRows.kind === "Confirm") {
    const { calls } = meta.allRows;
    const them = calls.length === 1 ? "that call" : "these calls";
    const run = await confirm({
      title: "Run the statement again?",
      body: `Exporting every row runs the statement again in a read-only transaction. It calls ${calls.join(", ")}. L1 Data can't vouch for ${them}, and a call can act outside the transaction.`,
      confirmLabel: "Run and export",
    });
    if (!run) return;
  }
  const { label, extension } = EXPORT_FORMATS[format];
  let path: string | null;
  try {
    path = await save({ defaultPath: `${isTableTab(tab) ? tab.table.table : "result"}.${extension}`, filters: [{ name: label, extensions: [extension] }] });
  } catch {
    notify("Export needs the desktop app's save dialog.");
    return;
  }
  if (!path) return;
  // For All, progress and Esc come from the console snapshot, not from this promise.
  const res = await commands.resultExport(meta.ref, format, scope, path);
  notify(res.status === "error" ? exportErrorText(res.error, path) : exportedText(res.data, path));
}

function exportedText(s: ExportSummary, path: string): string {
  const cut = s.truncated ? " The result stopped at the row limit, so the file holds the loaded rows only." : "";
  return `Exported ${pluralRows(s.rows)} to ${path}.${cut}`;
}
