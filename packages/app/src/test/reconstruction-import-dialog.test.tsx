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

    const accuracy = screen.getByLabelText(/Number accuracy/i);
    const tolerance = screen.getByLabelText(/Fidelity versus simplicity/i);
    expect((accuracy as HTMLSelectElement).value).toBe("4");
    expect((tolerance as HTMLInputElement).value).toBe("0.01");
    expect(screen.getByText("Recommended")).toBeTruthy();

    fireEvent.change(accuracy, { target: { value: "5" } });
    fireEvent.change(tolerance, { target: { value: "0.025" } });
    fireEvent.click(screen.getByRole("button", { name: "Reconstruct" }));

    expect(onConfirm).toHaveBeenCalledWith({
      decimalPlaces: 5,
      simplificationTolerance: 0.025,
    });
  });
});
