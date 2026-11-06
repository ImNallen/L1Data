// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { ConnectionDialog } from "./ConnectionDialog";

afterEach(() => {
  cleanup();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

it("keeps basic connection settings outside the advanced disclosure and preserves collapsed values", async () => {
  const user = userEvent.setup();
  useWorkspace.setState({ overlay: { kind: "connection", editing: null } });
  render(<ConnectionDialog />);
  const summary = screen.getByText("Advanced network settings");
  const details = summary.closest("details");
  expect(details?.open).toBe(false);
  expect(details?.contains(screen.getByLabelText("Name"))).toBe(false);
  expect(details?.contains(screen.getByLabelText("TLS"))).toBe(false);
  expect(document.activeElement).toBe(screen.getByTestId("conn-url"));

  await user.click(summary);
  await waitFor(() => expect(details?.open).toBe(true));
  await user.type(screen.getByLabelText("CA certificate"), "/tmp/database-ca.pem");
  await user.click(summary);
  await waitFor(() => expect(details?.open).toBe(false));
  await user.click(summary);
  await waitFor(() => expect(details?.open).toBe(true));
  expect(screen.getByTestId("conn-ca-cert").getAttribute("value")).toBe("/tmp/database-ca.pem");
});

it("opens configured network settings when editing a connection", () => {
  const config = { ...connectionConfig("c1", "Reporting"), caCertPath: "/tmp/database-ca.pem" };
  useWorkspace.setState({ configs: { c1: config }, overlay: { kind: "connection", editing: "c1" } });
  render(<ConnectionDialog />);
  expect(screen.getByText("Advanced network settings").closest("details")?.open).toBe(true);
  expect(document.activeElement).toBe(screen.getByLabelText("Name"));
  expect(screen.getByText("CA certificate configured")).toBeTruthy();
});
