import { useEffect, useState } from "react";
import * as RadixDialog from "@radix-ui/react-dialog";
import { Dialog, DialogContent, DialogFooter } from "@/components/ui/dialog";
import type { CadReconstructionOptions } from "@/lib/brep-reconstruct";

export const RECOMMENDED_RECONSTRUCTION_OPTIONS: CadReconstructionOptions = {
  decimalPlaces: 4,
  simplificationTolerance: 0.01,
};

interface ReconstructionImportDialogProps {
  fileName: string | null;
  onCancel: () => void;
  onConfirm: (options: CadReconstructionOptions) => void;
}

function toleranceLabel(value: number): string {
  return `${value.toFixed(value < 0.01 ? 3 : 2)} mm`;
}

export function ReconstructionImportDialog({
  fileName,
  onCancel,
  onConfirm,
}: ReconstructionImportDialogProps) {
  const [decimalPlaces, setDecimalPlaces] = useState(
    RECOMMENDED_RECONSTRUCTION_OPTIONS.decimalPlaces,
  );
  const [simplificationTolerance, setSimplificationTolerance] = useState(
    RECOMMENDED_RECONSTRUCTION_OPTIONS.simplificationTolerance,
  );

  useEffect(() => {
    if (!fileName) return;
    setDecimalPlaces(RECOMMENDED_RECONSTRUCTION_OPTIONS.decimalPlaces);
    setSimplificationTolerance(
      RECOMMENDED_RECONSTRUCTION_OPTIONS.simplificationTolerance,
    );
  }, [fileName]);

  const recommended =
    decimalPlaces === RECOMMENDED_RECONSTRUCTION_OPTIONS.decimalPlaces &&
    simplificationTolerance ===
      RECOMMENDED_RECONSTRUCTION_OPTIONS.simplificationTolerance;

  return (
    <Dialog
      open={fileName !== null}
      onOpenChange={(open) => {
        if (!open) onCancel();
      }}
    >
      <DialogContent title="Reconstruct as editable Loon" className="max-w-lg">
        <div className="space-y-5 text-xs text-text-muted">
          <div>
            <div
              className="truncate text-sm font-medium text-text"
              title={fileName ?? undefined}
            >
              {fileName}
            </div>
            <RadixDialog.Description className="mt-1 leading-relaxed">
              VCAD will infer native sketches, arcs, extrusions, holes, and one
              final part. The source file will not be embedded.
            </RadixDialog.Description>
          </div>

          <label className="block space-y-2">
            <div className="flex items-center justify-between">
              <span className="font-medium text-text">Number accuracy</span>
              <span>{decimalPlaces} decimal places</span>
            </div>
            <select
              value={decimalPlaces}
              onChange={(event) => setDecimalPlaces(Number(event.target.value))}
              className="h-9 w-full rounded border border-border bg-bg px-3 text-xs text-text outline-none focus:border-brand"
            >
              {Array.from({ length: 9 }, (_, value) => (
                <option key={value} value={value}>
                  {value} decimal place{value === 1 ? "" : "s"}
                </option>
              ))}
            </select>
            <p className="leading-relaxed">
              Coordinates are rounded before Loon is written. Four places
              removes mesh floating-point noise while retaining 0.0001 mm
              resolution.
            </p>
          </label>

          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3">
              <label
                htmlFor="reconstruction-tolerance"
                className="font-medium text-text"
              >
                Fidelity versus simplicity
              </label>
              <div className="flex items-center gap-2">
                {recommended && (
                  <span className="rounded bg-brand/15 px-1.5 py-0.5 text-[10px] font-medium text-brand">
                    Recommended
                  </span>
                )}
                <span className="tabular-nums text-text">
                  {toleranceLabel(simplificationTolerance)}
                </span>
              </div>
            </div>
            <input
              id="reconstruction-tolerance"
              type="range"
              min={0.001}
              max={0.25}
              step={0.001}
              value={simplificationTolerance}
              onChange={(event) =>
                setSimplificationTolerance(Number(event.target.value))
              }
              className="w-full accent-brand"
            />
            <div className="flex justify-between text-[10px] uppercase tracking-wide">
              <span>Closest to mesh</span>
              <span>Simplest object</span>
            </div>
            <p className="leading-relaxed">
              This is the maximum geometric deviation allowed when replacing
              mesh edges with a straight line or analytical arc. The initial
              0.01 mm setting is conservative but large enough to recover
              normally tessellated circles and fillets.
            </p>
            {!recommended && (
              <button
                type="button"
                onClick={() => {
                  setDecimalPlaces(
                    RECOMMENDED_RECONSTRUCTION_OPTIONS.decimalPlaces,
                  );
                  setSimplificationTolerance(
                    RECOMMENDED_RECONSTRUCTION_OPTIONS.simplificationTolerance,
                  );
                }}
                className="text-brand hover:underline"
              >
                Restore recommended settings
              </button>
            )}
          </div>
        </div>

        <DialogFooter>
          <button
            type="button"
            onClick={onCancel}
            className="h-8 rounded px-3 text-xs text-text-muted hover:bg-hover hover:text-text"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() =>
              onConfirm({ decimalPlaces, simplificationTolerance })
            }
            className="h-8 rounded bg-brand px-3 text-xs font-medium text-white hover:bg-brand-hover"
          >
            Reconstruct
          </button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
