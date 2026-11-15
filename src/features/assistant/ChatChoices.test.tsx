// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ChatChoices } from "./ChatChoices";

afterEach(cleanup);

it("waits for Continue and submits just the selected follow-ups", async () => {
  const user = userEvent.setup();
  const submit = vi.fn();
  render(<ChatChoices options={["By region", "By month", "By product"]} disabled={false} onContinue={submit} />);
  expect(screen.getByRole("button", { name: "Continue" })).toHaveProperty("disabled", true);
  await user.click(screen.getByRole("checkbox", { name: "By region" }));
  await user.click(screen.getByRole("checkbox", { name: "By month" }));
  expect(submit).not.toHaveBeenCalled();
  expect(screen.getByText("2 selected")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Continue" }));
  expect(submit).toHaveBeenCalledWith(["By region", "By month"]);
});

it("disables the choices and Continue during a turn", () => {
  const props = { options: ["By region", "By month"], onContinue: vi.fn() };
  render(<ChatChoices {...props} disabled />);
  expect(screen.getByRole("checkbox", { name: "By month" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "Continue" })).toHaveProperty("disabled", true);
});
