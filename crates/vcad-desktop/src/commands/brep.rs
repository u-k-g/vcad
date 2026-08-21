//! Thin desktop bridge to the repository's standalone mesh-to-Loon helper.
//!
//! The helper does not retain or embed source files. Unsupported topology
//! fails closed instead of silently falling back to an imported mesh.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;

const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 8 * 1024;

const RECONSTRUCT_SCRIPT: &str = include_str!("../../../../tools/mesh-to-loon.py");

#[derive(Debug, Deserialize)]
struct ReconstructionReport {
    axis: String,
    layers: usize,
    source_volume: f64,
    reconstructed_volume: f64,
    relative_volume_error: f64,
    face_count: usize,
    output_parts: usize,
    decimal_places: u8,
    simplification_tolerance: f64,
    input_segments: usize,
    output_segments: usize,
    recovered_arcs: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CadLoonResult {
    loon_source: String,
    axis: String,
    layers: usize,
    source_volume: f64,
    reconstructed_volume: f64,
    relative_volume_error: f64,
    face_count: usize,
    output_parts: usize,
    decimal_places: u8,
    simplification_tolerance: f64,
    input_segments: usize,
    output_segments: usize,
    recovered_arcs: usize,
}

fn freecad_resource_dir() -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os("FREECAD_RESOURCE_DIR") {
        let path = PathBuf::from(value);
        if path.join("bin/freecadcmd").is_file() && path.join("lib/Part.so").is_file() {
            return Ok(path);
        }
    }

    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from("/Applications/FreeCAD.app/Contents/Resources");
        if path.join("bin/freecadcmd").is_file() && path.join("lib/Part.so").is_file() {
            return Ok(path);
        }
    }

    Err("Native mesh-to-Loon reconstruction needs FreeCAD. Install FreeCAD, or set FREECAD_RESOURCE_DIR to its Resources directory.".to_string())
}

fn remove_file_best_effort(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "vcad: could not remove temporary file {}: {error}",
                path.display()
            );
        }
    }
}

fn bounded_error(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut detail = text.trim().to_string();
    if detail.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
        detail.push('…');
    }
    detail
}

#[tauri::command]
pub async fn reconstruct_cad_to_loon(
    app: tauri::AppHandle,
    data_base64: String,
    source_name: String,
    decimal_places: u8,
    simplification_tolerance: f64,
) -> Result<CadLoonResult, String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|_| "source payload is not valid base64".to_string())?;
    if data.is_empty() {
        return Err("source CAD file is empty".to_string());
    }
    if data.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source CAD file is too large ({} bytes; maximum is {MAX_SOURCE_BYTES})",
            data.len()
        ));
    }
    if source_name.len() > 512 {
        return Err("source filename is too long".to_string());
    }
    if decimal_places > 8 {
        return Err("numeric precision must be between 0 and 8 decimal places".to_string());
    }
    if !simplification_tolerance.is_finite()
        || !(0.000_001..=1.0).contains(&simplification_tolerance)
    {
        return Err(
            "simplification tolerance must be between 0.000001 and 1.0 model units".to_string(),
        );
    }

    let resource_dir = freecad_resource_dir()?;
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("could not locate VCAD cache directory: {error}"))?
        .join("mesh-to-loon");
    fs::create_dir_all(&cache_dir)
        .map_err(|error| format!("could not create reconstruction cache: {error}"))?;

    let digest = format!("{:x}", Sha256::digest(&data));
    // FreeCAD uses the extension to select the exchange-format reader. Keep
    // only the small allow-listed suffix; never place the supplied name in a
    // filesystem path.
    let source_extension = Path::new(&source_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| {
            matches!(
                extension.as_str(),
                "brep" | "brp" | "step" | "stp" | "stl" | "obj" | "3mf"
            )
        })
        .ok_or_else(|| {
            "native reconstruction expects BREP, BRP, STEP, STP, STL, OBJ, or 3MF".to_string()
        })?;
    let source_path = cache_dir.join(format!("{digest}.{source_extension}"));
    let script_path = cache_dir.join("mesh-to-loon.py");
    let loon_path = cache_dir.join(format!("{digest}.loon"));
    let report_path = cache_dir.join(format!("{digest}.json"));

    fs::write(&source_path, data)
        .map_err(|error| format!("could not stage mesh-to-Loon reconstruction: {error}"))?;
    fs::write(&script_path, RECONSTRUCT_SCRIPT)
        .map_err(|error| format!("could not stage reconstruction script: {error}"))?;
    remove_file_best_effort(&loon_path);
    remove_file_best_effort(&report_path);

    let executable = resource_dir.join("bin/freecadcmd");
    let source_value = source_path.to_string_lossy().into_owned();
    let loon_value = loon_path.to_string_lossy().into_owned();
    let report_value = report_path.to_string_lossy().into_owned();
    let command_result = tauri::async_runtime::spawn_blocking(move || {
        Command::new(executable)
            .arg(script_path)
            .env("VCAD_MESH_TO_LOON_SOURCE", source_value)
            .env("VCAD_MESH_TO_LOON_OUTPUT", loon_value)
            .env("VCAD_MESH_TO_LOON_REPORT", report_value)
            .env("VCAD_MESH_TO_LOON_SOURCE_NAME", source_name)
            .env(
                "VCAD_MESH_TO_LOON_DECIMAL_PLACES",
                decimal_places.to_string(),
            )
            .env(
                "VCAD_MESH_TO_LOON_SIMPLIFICATION_TOLERANCE",
                simplification_tolerance.to_string(),
            )
            .output()
    })
    .await
    .map_err(|error| format!("FreeCAD reconstruction task failed: {error}"))?;
    remove_file_best_effort(&source_path);

    let output = command_result
        .map_err(|error| format!("could not start FreeCAD reconstruction: {error}"))?;
    if !output.status.success() {
        remove_file_best_effort(&loon_path);
        remove_file_best_effort(&report_path);
        let stderr = bounded_error(&output.stderr);
        let stdout = bounded_error(&output.stdout);
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            format!("native reconstruction failed with {}", output.status)
        } else {
            format!("native reconstruction failed: {detail}")
        });
    }
    // FreeCADCmd reports Python script exceptions on stdout but may still
    // return exit status 0. Treat missing result artifacts as a failed job and
    // preserve its actionable exception text for the UI.
    if !loon_path.is_file() || !report_path.is_file() {
        let stderr = bounded_error(&output.stderr);
        let stdout = bounded_error(&output.stdout);
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            "native reconstruction did not produce Loon output".to_string()
        } else {
            format!("native reconstruction failed: {detail}")
        });
    }

    let loon_source = fs::read_to_string(&loon_path)
        .map_err(|error| format!("could not read reconstructed Loon source: {error}"))?;
    let report: ReconstructionReport = serde_json::from_slice(
        &fs::read(&report_path)
            .map_err(|error| format!("could not read reconstruction report: {error}"))?,
    )
    .map_err(|error| format!("invalid reconstruction report: {error}"))?;
    if loon_source.trim().is_empty() {
        return Err("FreeCAD produced empty Loon source".to_string());
    }

    Ok(CadLoonResult {
        loon_source,
        axis: report.axis,
        layers: report.layers,
        source_volume: report.source_volume,
        reconstructed_volume: report.reconstructed_volume,
        relative_volume_error: report.relative_volume_error,
        face_count: report.face_count,
        output_parts: report.output_parts,
        decimal_places: report.decimal_places,
        simplification_tolerance: report.simplification_tolerance,
        input_segments: report.input_segments,
        output_segments: report.output_segments,
        recovered_arcs: report.recovered_arcs,
    })
}

#[cfg(test)]
mod tests {
    use super::{bounded_error, freecad_resource_dir, MAX_SOURCE_BYTES};

    #[test]
    fn reconstruction_limits_are_bounded() {
        assert_eq!(MAX_SOURCE_BYTES, 256 * 1024 * 1024);
        assert!(bounded_error(&vec![b'x'; 16 * 1024]).len() < 9 * 1024);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_installed_freecad_runtime_when_present() {
        let expected = std::path::Path::new("/Applications/FreeCAD.app/Contents/Resources");
        if expected.is_dir() {
            assert_eq!(freecad_resource_dir().unwrap(), expected);
        }
    }
}
