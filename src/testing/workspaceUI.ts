import { screen } from "@testing-library/react";
import type userEvent from "@testing-library/user-event";

export async function openWorkspaceItem(user: ReturnType<typeof userEvent.setup>, testId: string) {
  await user.click(screen.getByTestId(testId));
}
