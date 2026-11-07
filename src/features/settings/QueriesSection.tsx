import { useState } from "react";
import { useWorkspace } from "../../state/workspace";
import { cx, inputClass } from "../../ui/primitives";
import { saveSettings } from "../editor/runActions";
import { Row } from "./SettingLayout";

function NumberInput({ id, field, value }: { id: string; field: "maxRows" | "tablePageSize"; value: number }) {
  const [draft, setDraft] = useState(String(value));
  const commit = () => {
    const n = Math.round(Number(draft));
    if (draft.trim() === "" || !Number.isFinite(n)) return setDraft(String(value));
    const next = Math.max(1, n);
    setDraft(String(next));
    if (next !== value) void saveSettings({ [field]: next });
  };
  return (
    <div className="w-28 shrink-0">
      <input
        id={id}
        data-testid={`setting-${field}`}
        type="number"
        min={1}
        className={cx(inputClass, "text-right tabular-nums")}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape" && draft !== String(value)) {
            e.stopPropagation();
            setDraft(String(value));
          }
        }}
      />
    </div>
  );
}

export function MaxRowsSetting() {
  const maxRows = useWorkspace((s) => s.settings.maxRows);
  return (
    <Row
      setting="maxRows"
      labelFor="setting-max-rows"
      hint="Rows past this are counted but not kept."
      control={<NumberInput key={maxRows} id="setting-max-rows" field="maxRows" value={maxRows} />}
    />
  );
}

export function TablePageSizeSetting() {
  const tablePageSize = useWorkspace((s) => s.settings.tablePageSize);
  return (
    <Row
      setting="tablePageSize"
      labelFor="setting-page-size"
      hint="Rows per page in query results and table tabs."
      control={<NumberInput key={tablePageSize} id="setting-page-size" field="tablePageSize" value={tablePageSize} />}
    />
  );
}

export function StopOnErrorSetting() {
  const stopOnError = useWorkspace((s) => s.settings.stopOnError);
  return (
    <Row
      setting="stopOnError"
      labelFor="setting-stop-on-error"
      hint="Later statements are skipped once one fails."
      control={
        <input
          id="setting-stop-on-error"
          data-testid="setting-stopOnError"
          type="checkbox"
          className="size-4 shrink-0 accent-run"
          checked={stopOnError}
          onChange={(e) => void saveSettings({ stopOnError: e.target.checked })}
        />
      }
    />
  );
}
