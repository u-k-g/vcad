//! Narrow in-process OpenCascade bridge for decoding exchange BReps.
//!
//! OCCT is only used to decode and tessellate its native BREP container (and
//! STEP files whose independent VCAD face tessellation does not sew). Feature
//! recognition and all emitted VCAD/Loon geometry remain implemented in Rust.

use std::io::Write;

use opencascade_sys::ffi as occ;

use crate::{Mesh, ReconstructionError, Triangle};

#[cxx::bridge(namespace = "vcad_reconstruct")]
mod bridge {
    unsafe extern "C++" {
        include!("occ_bridge.hxx");

        #[namespace = ""]
        type TopoDS_Shape = opencascade_sys::ffi::TopoDS_Shape;

        fn read_brep(path: &str) -> UniquePtr<TopoDS_Shape>;
    }
}

pub(crate) fn parse_brep(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let mut file = tempfile::Builder::new()
        .prefix("vcad-reconstruct-")
        .suffix(".brep")
        .tempfile()
        .map_err(|error| parse_error("BREP", error))?;
    file.write_all(data)
        .and_then(|_| file.flush())
        .map_err(|error| parse_error("BREP", error))?;
    let path = file.path().to_string_lossy();
    let shape = bridge::read_brep(&path);
    if shape.is_null() {
        return Err(ReconstructionError::Parse {
            format: "BREP",
            detail: "OpenCascade rejected the topology stream".into(),
        });
    }
    tessellate_shape(shape.as_ref().expect("checked non-null"), "BREP")
}

pub(crate) fn parse_step(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let mut file = tempfile::Builder::new()
        .prefix("vcad-reconstruct-")
        .suffix(".step")
        .tempfile()
        .map_err(|error| parse_error("STEP", error))?;
    file.write_all(data)
        .and_then(|_| file.flush())
        .map_err(|error| parse_error("STEP", error))?;

    let mut reader = occ::STEPControl_Reader_ctor();
    let status = occ::read_step(reader.pin_mut(), file.path().to_string_lossy().into_owned());
    if status != occ::IFSelect_ReturnStatus::IFSelect_RetDone {
        return Err(ReconstructionError::Parse {
            format: "STEP",
            detail: format!("OpenCascade reader returned {status:?}"),
        });
    }
    let progress = occ::Message_ProgressRange_ctor();
    let transferred = reader.pin_mut().TransferRoots(&progress);
    if transferred != 1 {
        return Err(ReconstructionError::InvalidMesh(format!(
            "expected one STEP body, transferred {transferred}; import bodies separately"
        )));
    }
    let shape = occ::one_shape(&reader);
    if shape.is_null() {
        return Err(ReconstructionError::Parse {
            format: "STEP",
            detail: "reader produced no shape".into(),
        });
    }
    tessellate_shape(shape.as_ref().expect("checked non-null"), "STEP")
}

fn tessellate_shape(
    shape: &occ::TopoDS_Shape,
    format: &'static str,
) -> Result<Mesh, ReconstructionError> {
    // Keep chord error comfortably below the default 0.01 reconstruction
    // tolerance so adjacent analytical faces yield sewable section endpoints.
    let mesher = occ::BRepMesh_IncrementalMesh_ctor(shape, 0.001);
    if !mesher.IsDone() {
        return Err(ReconstructionError::Parse {
            format,
            detail: "OpenCascade tessellation did not complete".into(),
        });
    }

    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let mut explorer =
        occ::TopExp_Explorer_ctor(mesher.Shape(), occ::TopAbs_ShapeEnum::TopAbs_FACE);
    while explorer.More() {
        let face_shape = explorer.Current();
        let face = occ::TopoDS_cast_to_face(face_shape);
        let reversed = face.Orientation() == occ::TopAbs_Orientation::TopAbs_REVERSED;
        let mut location = occ::TopLoc_Location_ctor();
        let handle = occ::BRep_Tool_Triangulation(face, location.pin_mut());
        if !handle.IsNull() {
            let triangulation = occ::Handle_Poly_Triangulation_Get(&handle).map_err(|error| {
                ReconstructionError::Parse {
                    format,
                    detail: format!("could not access face triangulation: {error}"),
                }
            })?;
            let offset = vertices.len() as u32;
            let transform = occ::TopLoc_Location_Transformation(&location);
            for index in 1..=triangulation.NbNodes() {
                let mut point = occ::Poly_Triangulation_Node(triangulation, index);
                point.pin_mut().Transform(&transform);
                vertices.push([point.X(), point.Y(), point.Z()]);
            }
            for index in 1..=triangulation.NbTriangles() {
                let triangle = triangulation.Triangle(index);
                let a = offset + triangle.Value(1) as u32 - 1;
                let b = offset + triangle.Value(2) as u32 - 1;
                let c = offset + triangle.Value(3) as u32 - 1;
                triangles.push(if reversed {
                    Triangle([c, b, a])
                } else {
                    Triangle([a, b, c])
                });
            }
        }
        explorer.pin_mut().Next();
    }
    if vertices.is_empty() || triangles.is_empty() {
        return Err(ReconstructionError::Parse {
            format,
            detail: "shape contains no tessellatable faces".into(),
        });
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

fn parse_error(format: &'static str, error: impl std::fmt::Display) -> ReconstructionError {
    ReconstructionError::Parse {
        format,
        detail: error.to_string(),
    }
}
