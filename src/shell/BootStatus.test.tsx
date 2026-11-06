// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { BootStatus } from "./BootStatus";

afterEach(cleanup);

it("announces startup while the app waits for the engine", () => {
  render(<BootStatus state={{ kind: "starting" }} />);
  expect(screen.getByRole("status").textContent).toContain("Starting L1 Data");
  expect(screen.queryByRole("button")).toBeNull();
});

it("shows the startup error and lets the user retry", () => {
  const retry = vi.fn();
  render(<BootStatus state={{ kind: "failed", error: "Could not read workspace", bridge: false, retry }} />);
  expect(screen.getByRole("heading").textContent).toBe("Could not start L1 Data");
  expect(screen.getByRole("alert").textContent).toBe("Could not read workspace");
  expect(screen.queryByText(/engine may still be starting/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(retry).toHaveBeenCalledOnce();
});

it("explains that the browser bridge may still be starting", () => {
  render(<BootStatus state={{ kind: "failed", error: "Failed to fetch", bridge: true, retry: vi.fn() }} />);
  expect(screen.getByText(/engine may still be starting/)).toBeTruthy();
});
