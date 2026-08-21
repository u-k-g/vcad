//! Desktop bridge to VCAD's in-process Rust feature reconstructor.
//!
//! No source file is retained or embedded, and no Python/FreeCAD/external
//! process is involved. Unsupported topology fails closed.

use base64::Engine;
use serde::Serialize;

use vcad_kernel_reconstruct::{reconstruct_with_resource_dir, ReconstructionOptions};

const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CadLoonResult {
    loon_source: String,
    axis: String,
    feature_count: usize,
    body_count: usize,
    source_volume: f64,
    reconstructed_volume: f64,
    relative_volume_error: f64,
    source_triangles: usize,
    output_segments: usize,
    recovered_arcs: usize,
    fillets: usize,
    chamfers: usize,
    decimal_places: u8,
    simplification_tolerance: f64,
    max_profile_deviation: f64,
}

#[tauri::command]
pub async fn reconstruct_cad_to_loon(
    data_base64: String,
    source_name: String,
    source_path: Option<String>,
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

    let result = tauri::async_runtime::spawn_blocking(move || {
        let resource_dir = source_path
            .as_deref()
            .and_then(|path| std::path::Path::new(path).parent());
        reconstruct_with_resource_dir(
            &data,
            &source_name,
            ReconstructionOptions {
                decimal_places,
                tolerance: simplification_tolerance,
            },
            resource_dir,
        )
    })
    .await
    .map_err(|error| format!("native reconstruction task failed: {error}"))?
    .map_err(|error| error.to_string())?;

    let report = result.report;
    Ok(CadLoonResult {
        loon_source: result.loon_source,
        axis: report.axis,
        feature_count: report.feature_count,
        body_count: report.body_count,
        source_volume: report.source_volume,
        reconstructed_volume: report.reconstructed_volume,
        relative_volume_error: report.relative_volume_error,
        source_triangles: report.source_triangles,
        output_segments: report.output_segments,
        recovered_arcs: report.recovered_arcs,
        fillets: report.fillets,
        chamfers: report.chamfers,
        decimal_places: report.decimal_places,
        simplification_tolerance: report.simplification_tolerance,
        max_profile_deviation: report.max_profile_deviation,
    })
}

#[cfg(test)]
mod tests {
    use super::MAX_SOURCE_BYTES;

    #[test]
    fn reconstruction_payload_limit_is_bounded() {
        assert_eq!(MAX_SOURCE_BYTES, 256 * 1024 * 1024);
    }
}
