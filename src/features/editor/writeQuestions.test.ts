import { describe, expect, it } from "vitest";
import type { WriteConfirmation } from "../../ipc/bindings";
import { productionQuestion } from "./writeQuestions";

const asked = (patch: Partial<WriteConfirmation> = {}): WriteConfirmation => ({
  connection: "prod-postgres",
  database: "shop",
  unit: "Statement",
  sends: { count: 0, statements: [] },
  holds: { count: 0, statements: [] },
  digest: "d",
  onProduction: true,
  ...patch,
});

describe("productionQuestion", () => {
  it("asks nothing when no cell would ask", () => {
    expect(productionQuestion([])).toBeNull();
  });

  it("asks once for every writing cell, with a row for each", () => {
    const question = productionQuestion([
      { title: "load", confirmation: asked({ sends: { count: 3, statements: ["insert into a values (1)"] } }) },
      { title: "fix", confirmation: asked({ sends: { count: 1, statements: ["update a set n = 2"] }, holds: { count: 2, statements: [] } }) },
    ]);
    expect(question).toEqual({
      title: "Run 4 writes on Production?",
      body: "This run sends 4 statements that write to prod-postgres / shop from 2 cells. 2 writes to prod-postgres / shop ran in these cells before the connection was marked Production. If they are still in a cell's transaction, confirming covers them, so a later commit does not ask about them again.",
      rows: [
        { label: "load", detail: "3 writes" },
        { label: "fix", detail: "1 write · 2 held" },
      ],
      note: "A cell that changes before its turn asks again.",
      confirmLabel: "Run writes",
    });
  });

  it("names SQL Server batches, and asks to run when only held writes ask", () => {
    expect(productionQuestion([{ title: "purge", confirmation: asked({ unit: "Batch", sends: { count: 1, statements: ["EXEC dbo.purge"] } }) }])).toMatchObject(
      {
        body: "This run sends 1 batch that writes to prod-postgres / shop.",
        rows: [{ label: "purge", detail: "1 batch" }],
      },
    );
    expect(productionQuestion([{ title: "read", confirmation: asked({ holds: { count: 1, statements: [] } }) }])).toMatchObject({
      title: "Run on Production?",
      rows: [{ label: "read", detail: "1 held" }],
      confirmLabel: "Run",
    });
  });
});
