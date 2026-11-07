import { useId, useState } from "react";

export function ChatChoices({
  options,
  disabled,
  multiple = false,
  onContinue,
}: {
  options: string[];
  disabled: boolean;
  multiple?: boolean;
  onContinue(options: string[]): void;
}) {
  const group = useId();
  const [selected, setSelected] = useState<string[]>([]);
  return (
    <div className="chat-card flex flex-col gap-2 p-3" role="group" aria-labelledby={group}>
      <p id={group} className="text-ui-13 leading-[22px]">
        What would you like to explore next?
      </p>
      <p className="text-ui-11 text-dim">{multiple ? "Select all that apply" : "Select one"}</p>
      <div className="flex flex-col gap-1">
        {options.map((option) => (
          <label key={option} className="chat-choice cursor-pointer">
            <input
              type={multiple ? "checkbox" : "radio"}
              name={group}
              value={option}
              checked={selected.includes(option)}
              disabled={disabled}
              onChange={(e) => setSelected((before) => (multiple ? (e.target.checked ? [...before, option] : before.filter((s) => s !== option)) : [option]))}
            />
            <span className="min-w-0 break-words">{option}</span>
          </label>
        ))}
      </div>
      <div className="flex items-center justify-end gap-2 pt-1">
        {multiple && <span className="mr-auto text-ui-11 text-muted">{selected.length} selected</span>}
        <button type="button" className="chat-button" disabled={disabled || selected.length === 0} onClick={() => onContinue(selected)}>
          Continue
        </button>
      </div>
    </div>
  );
}
