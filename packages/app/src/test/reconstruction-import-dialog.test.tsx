import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ReconstructionImportDialog } from "@/components/ReconstructionImportDialog";

afterEach(cleanup);

describe("ReconstructionImportDialog", () => {
  it("starts detail-preserving and returns the selected real tolerances", () => {
    const onConfirm = vi.fn();
    render(
      <ReconstructionImportDialog
        fileName="part.3mf"
        onCancel={vi.fn()}
        onConfirm={onConfirm}
      />,
    );

    const tolerance = screen.getByLabelText(/Fidelity versus simplicity/i);
    expect(screen.getByLabelText("Number preview").textContent).toBe("9.9999");
    expect(screen.getByLabelText("Decimal places").textContent).toContain("4");
    expect((tolerance as HTMLInputElement).value).toBe("0.01");
    expect(screen.getAllByText("Recommended")).toHaveLength(2);

    fireEvent.click(
      screen.getByRole("button", { name: "Increase decimal places" }),
    );
    expect(screen.getByLabelText("Number preview").textContent).toBe("9.99999");
    expect(
      screen.getByRole("button", { name: "Reset to recommended" }),
    ).toBeTruthy();
    fireEvent.change(tolerance, { target: { value: "0.025" } });
    expect(
      screen.getAllByRole("button", { name: "Reset to recommended" }),
    ).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "Reconstruct" }));

    expect(onConfirm).toHaveBeenCalledWith({
      decimalPlaces: 5,
      simplificationTolerance: 0.025,
    });
  });
});
