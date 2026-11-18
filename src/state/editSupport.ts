import { create } from "zustand";
import { commands, type ApplyError, type EditSupport, type TabId } from "../ipc/bindings";

/** A SQL Server table tab's edit support, for the table and catalog version `key` names. Submit checks again. */
export type SupportEntry = { key: string; support: EditSupport | null; error: ApplyError | null };

export const useEditSupport = create<{ byTab: Record<TabId, SupportEntry> }>(() => ({ byTab: {} }));

const put = (tab: TabId, entry: SupportEntry) => useEditSupport.setState((s) => ({ byTab: { ...s.byTab, [tab]: entry } }));

export async function loadEditSupport(tab: TabId, schema: string, table: string, key: string): Promise<void> {
  if (useEditSupport.getState().byTab[tab]?.key === key) return;
  put(tab, { key, support: null, error: null });
  const res = await commands.tableEditSupport(tab, schema, table);
  if (useEditSupport.getState().byTab[tab]?.key !== key) return;
  put(tab, res.status === "ok" ? { key, support: res.data, error: null } : { key, support: null, error: res.error });
}

/** Drops a tab's answer, so the results pane asks again. */
export function forgetEditSupport(tab: TabId): void {
  useEditSupport.setState((s) => {
    const { [tab]: _, ...rest } = s.byTab;
    return { byTab: rest };
  });
}
