import { invoke, isTauri } from "@/lib/tauri";

export interface CadReconstructionResult {
  loonSource: string;
  axis: "x" | "y" | "z";
  layers: number;
  sourceVolume: number;
  reconstructedVolume: number;
  relativeVolumeError: number;
  faceCount: number;
  outputParts: number;
  decimalPlaces: number;
  simplificationTolerance: number;
  inputSegments: number;
  outputSegments: number;
  recoveredArcs: number;
}

export interface CadReconstructionOptions {
  decimalPlaces: number;
  simplificationTolerance: number;
}

function arrayBufferToBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunkSize = 32 * 1024;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(
      ...bytes.subarray(offset, offset + chunkSize),
    );
  }
  return btoa(binary);
}

/** Reconstruct supported CAD/mesh geometry as self-contained native Loon source. */
export async function reconstructCadToLoon(
  buffer: ArrayBuffer,
  sourceName: string,
  options: CadReconstructionOptions,
): Promise<CadReconstructionResult> {
  if (!isTauri()) {
    throw new Error(
      "Native BREP/STEP/STL/OBJ/3MF reconstruction is desktop-only",
    );
  }
  return invoke<CadReconstructionResult>("reconstruct_cad_to_loon", {
    dataBase64: arrayBufferToBase64(buffer),
    sourceName,
    decimalPlaces: options.decimalPlaces,
    simplificationTolerance: options.simplificationTolerance,
  });
}
