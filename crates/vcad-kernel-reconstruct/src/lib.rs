#![warn(missing_docs)]

//! Native, fail-closed feature reconstruction for VCAD.
//!
//! This crate deliberately has no FreeCAD, Python, or external process
//! dependency. Exchange files are parsed in-process and reconstructed
//! as self-contained Loon operations. The recognizer describes a compact
//! feature history (base extrusion, additive/subtractive extrusions, and
//! analytical curves); it never emits one extrusion for every mesh depth
//! interval.

mod mesh;
mod mesh_formats;
mod occ;
mod parse;
mod profile;
mod scene_formats;

use serde::{Deserialize, Serialize};
use std::path::Path;

pub use mesh::{Mesh, Triangle};

/// Supported source container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// ISO 10303 STEP (`.step` or `.stp`).
    Step,
    /// STL triangle mesh.
    Stl,
    /// Wavefront OBJ triangle mesh.
    Obj,
    /// 3MF triangle mesh container.
    ThreeMf,
    /// Stanford Polygon File Format (`.ply`).
    Ply,
    /// Binary or JSON glTF scene (`.glb` or `.gltf`).
    Gltf,
    /// Object File Format (`.off`).
    Off,
    /// Additive Manufacturing Format (`.amf`).
    Amf,
}

impl SourceFormat {
    /// Resolve a supported format from a filename extension.
    pub fn from_filename(name: &str) -> Result<Self, ReconstructionError> {
        let extension = name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "step" | "stp" => Ok(Self::Step),
            "stl" => Ok(Self::Stl),
            "obj" => Ok(Self::Obj),
            "3mf" => Ok(Self::ThreeMf),
            "ply" => Ok(Self::Ply),
            "glb" | "gltf" => Ok(Self::Gltf),
            "off" => Ok(Self::Off),
            "amf" => Ok(Self::Amf),
            _ => Err(ReconstructionError::UnsupportedFormat(extension)),
        }
    }
}

/// User-controlled reconstruction settings.
#[derive(Debug, Clone, Copy)]
pub struct ReconstructionOptions {
    /// Decimal places emitted in Loon source.
    pub decimal_places: u8,
    /// Maximum permitted source-to-analytic contour deviation, in model units.
    pub tolerance: f64,
}

impl Default for ReconstructionOptions {
    fn default() -> Self {
        Self {
            decimal_places: 4,
            tolerance: 0.01,
        }
    }
}

/// Machine-readable reconstruction diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconstructionReport {
    /// Principal base-extrusion axis (`x`, `y`, or `z`).
    pub axis: String,
    /// Number of native modeling features emitted.
    pub feature_count: usize,
    /// Number of final solid bodies.
    pub body_count: usize,
    /// Signed source mesh volume in cubic model units.
    pub source_volume: f64,
    /// Reconstructed volume when available.
    pub reconstructed_volume: f64,
    /// Relative volume error.
    pub relative_volume_error: f64,
    /// Number of source triangles.
    pub source_triangles: usize,
    /// Number of analytical profile segments emitted.
    pub output_segments: usize,
    /// Number of tessellated curves recovered as analytical arcs.
    pub recovered_arcs: usize,
    /// Number of recognized fillet features.
    pub fillets: usize,
    /// Number of recognized chamfer features.
    pub chamfers: usize,
    /// Numeric precision used for source generation.
    pub decimal_places: u8,
    /// Geometric fitting tolerance.
    pub simplification_tolerance: f64,
    /// Maximum measured profile deviation.
    pub max_profile_deviation: f64,
}

/// Self-contained native reconstruction.
#[derive(Debug, Clone)]
pub struct Reconstruction {
    /// Generated Loon source.
    pub loon_source: String,
    /// Validation and feature-recognition diagnostics.
    pub report: ReconstructionReport,
}

/// Fail-closed reconstruction error.
#[derive(Debug, thiserror::Error)]
pub enum ReconstructionError {
    /// The source extension is unknown.
    #[error("unsupported reconstruction format: {0}")]
    UnsupportedFormat(String),
    /// Input decoding failed.
    #[error("could not parse {format}: {detail}")]
    Parse {
        /// Friendly source format.
        format: &'static str,
        /// Parser detail.
        detail: String,
    },
    /// The mesh is empty or invalid.
    #[error("invalid source mesh: {0}")]
    InvalidMesh(String),
    /// The recognizer cannot yet prove an accurate native feature history.
    #[error("native feature reconstruction refused this model: {0}")]
    UnresolvedFeatures(String),
    /// Settings are outside supported bounds.
    #[error("invalid reconstruction options: {0}")]
    InvalidOptions(String),
}

/// Reconstruct an exchange file as self-contained Loon source.
pub fn reconstruct(
    data: &[u8],
    source_name: &str,
    options: ReconstructionOptions,
) -> Result<Reconstruction, ReconstructionError> {
    reconstruct_with_resource_dir(data, source_name, options, None)
}

/// Reconstruct an exchange file and resolve glTF companion buffers beneath
/// `resource_dir`. Other formats ignore the directory.
pub fn reconstruct_with_resource_dir(
    data: &[u8],
    source_name: &str,
    options: ReconstructionOptions,
    resource_dir: Option<&Path>,
) -> Result<Reconstruction, ReconstructionError> {
    if options.decimal_places > 8 {
        return Err(ReconstructionError::InvalidOptions(
            "decimal places must be between 0 and 8".into(),
        ));
    }
    if !options.tolerance.is_finite() || !(1.0e-6..=1.0).contains(&options.tolerance) {
        return Err(ReconstructionError::InvalidOptions(
            "tolerance must be between 0.000001 and 1.0 model units".into(),
        ));
    }

    let format = SourceFormat::from_filename(source_name)?;
    let mesh = parse::parse(data, format, resource_dir)?;
    mesh.validate(options.tolerance)?;
    profile::reconstruct_feature_model(&mesh, source_name, options)
}

#[cfg(test)]
mod tests {
    use super::{reconstruct, ReconstructionOptions};

    const CUBE_OBJ: &[u8] = br#"
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
f 4 3 2 1
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
"#;

    const CUBE_OFF: &[u8] = b"OFF\n8 6 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n0 0 1\n1 0 1\n1 1 1\n0 1 1\n4 3 2 1 0\n4 4 5 6 7\n4 0 1 5 4\n4 1 2 6 5\n4 2 3 7 6\n4 3 0 4 7\n";

    const CUBE_PLY: &[u8] = b"ply\nformat ascii 1.0\nelement vertex 8\nproperty float x\nproperty float y\nproperty float z\nelement face 6\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n0 0 1\n1 0 1\n1 1 1\n0 1 1\n4 3 2 1 0\n4 4 5 6 7\n4 0 1 5 4\n4 1 2 6 5\n4 2 3 7 6\n4 3 0 4 7\n";

    const CUBE_AMF: &[u8] = br#"<amf unit="millimeter"><object id="0"><mesh><vertices>
<vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex>
<vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex>
<vertex><coordinates><x>1</x><y>1</y><z>0</z></coordinates></vertex>
<vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex>
<vertex><coordinates><x>0</x><y>0</y><z>1</z></coordinates></vertex>
<vertex><coordinates><x>1</x><y>0</y><z>1</z></coordinates></vertex>
<vertex><coordinates><x>1</x><y>1</y><z>1</z></coordinates></vertex>
<vertex><coordinates><x>0</x><y>1</y><z>1</z></coordinates></vertex>
</vertices><volume>
<triangle><v1>3</v1><v2>2</v2><v3>1</v3></triangle><triangle><v1>3</v1><v2>1</v2><v3>0</v3></triangle>
<triangle><v1>4</v1><v2>5</v2><v3>6</v3></triangle><triangle><v1>4</v1><v2>6</v2><v3>7</v3></triangle>
<triangle><v1>0</v1><v2>1</v2><v3>5</v3></triangle><triangle><v1>0</v1><v2>5</v2><v3>4</v3></triangle>
<triangle><v1>1</v1><v2>2</v2><v3>6</v3></triangle><triangle><v1>1</v1><v2>6</v2><v3>5</v3></triangle>
<triangle><v1>2</v1><v2>3</v2><v3>7</v3></triangle><triangle><v1>2</v1><v2>7</v2><v3>6</v3></triangle>
<triangle><v1>3</v1><v2>0</v2><v3>4</v3></triangle><triangle><v1>3</v1><v2>4</v2><v3>7</v3></triangle>
</volume></mesh></object></amf>"#;

    #[test]
    fn reconstructs_closed_obj_as_one_native_body() {
        let reconstruction = reconstruct(
            CUBE_OBJ,
            "cube.obj",
            ReconstructionOptions {
                decimal_places: 4,
                tolerance: 0.001,
            },
        )
        .unwrap();
        assert_eq!(reconstruction.report.body_count, 1);
        assert_eq!(reconstruction.loon_source.matches("[root ").count(), 1);
        assert!(reconstruction.loon_source.contains("base-extrusion"));
        assert!(reconstruction.report.relative_volume_error < 1.0e-10);
    }

    #[test]
    fn reconstructs_new_triangle_containers_as_native_bodies() {
        for (data, name) in [
            (CUBE_PLY, "cube.ply"),
            (CUBE_OFF, "cube.off"),
            (CUBE_AMF, "cube.amf"),
        ] {
            let reconstruction = reconstruct(
                data,
                name,
                ReconstructionOptions {
                    decimal_places: 4,
                    tolerance: 0.001,
                },
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(reconstruction.report.body_count, 1, "{name}");
            assert_eq!(reconstruction.loon_source.matches("[root ").count(), 1);
        }
    }

    #[test]
    fn resolves_all_supported_extensions() {
        use super::SourceFormat;

        for name in [
            "shape.step",
            "shape.stp",
            "shape.stl",
            "shape.obj",
            "shape.3mf",
            "shape.ply",
            "shape.glb",
            "shape.gltf",
            "shape.off",
            "shape.amf",
        ] {
            assert!(SourceFormat::from_filename(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_removed_brep_container_extensions() {
        use super::SourceFormat;

        for name in ["shape.brep", "shape.brp"] {
            assert!(SourceFormat::from_filename(name).is_err(), "{name}");
        }
    }
}
