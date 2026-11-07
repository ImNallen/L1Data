// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ChatChoices } from "./ChatChoices";

afterEach(cleanup);

it("waits for Continue and submits just the selected follow-ups", async () => {
  const user = userEvent.setup();
  const submit = vi.fn();
  render(<ChatChoices multiple options={["By region", "By month", "By product"]} disabled={false} onContinue={submit} />);
  expect(screen.getByRole("button", { name: "Continue" })).toHaveProperty("disabled", true);
  await user.click(screen.getByRole("checkbox", { name: "By region" }));
  await user.click(screen.getByRole("checkbox", { name: "By month" }));
  expect(submit).not.toHaveBeenCalled();
  expect(screen.getByText("2 selected")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Continue" }));
  expect(submit).toHaveBeenCalledWith(["By region", "By month"]);
});

it("keeps a single-choice prompt exclusive and disables it during a turn", async () => {
  const user = userEvent.setup();
  const submit = vi.fn();
  const props = { options: ["By region", "By month"], onContinue: submit };
  const view = render(<ChatChoices {...props} disabled={false} />);
  await user.click(screen.getByRole("radio", { name: "By region" }));
  await user.click(screen.getByRole("radio", { name: "By month" }));
  expect(screen.getByRole("radio", { name: "By region" })).toHaveProperty("checked", false);
  view.rerender(<ChatChoices {...props} disabled />);
  expect(screen.getByRole("radio", { name: "By month" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "Continue" })).toHaveProperty("disabled", true);
});
