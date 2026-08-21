//! Scene-aware triangle decoders.

use base64::Engine;
use gltf::buffer::Source;
use gltf::mesh::Mode;
use std::path::Path;

use crate::{Mesh, ReconstructionError, Triangle};

pub(crate) fn parse_gltf(
    data: &[u8],
    resource_dir: Option<&Path>,
) -> Result<Mesh, ReconstructionError> {
    let gltf = gltf::Gltf::from_slice(data).map_err(|error| parse_error(error.to_string()))?;
    let buffers =
        gltf.buffers()
            .map(|buffer| match buffer.source() {
                Source::Bin => gltf.blob.clone().ok_or_else(|| {
                    parse_error("GLB declares a binary buffer but has no BIN chunk")
                }),
                Source::Uri(uri) => decode_buffer_uri(uri, resource_dir),
            })
            .collect::<Result<Vec<_>, _>>()?;
    for buffer in gltf.buffers() {
        let actual = buffers.get(buffer.index()).map_or(0, Vec::len);
        if actual < buffer.length() {
            return Err(parse_error(format!(
                "buffer {} declares {} bytes but only {actual} are available",
                buffer.index(),
                buffer.length()
            )));
        }
    }

    let scene = gltf
        .default_scene()
        .or_else(|| gltf.scenes().next())
        .ok_or_else(|| parse_error("document has no scene"))?;
    let mut mesh = Mesh {
        vertices: Vec::new(),
        triangles: Vec::new(),
    };
    for node in scene.nodes() {
        append_node(node, identity(), &buffers, &mut mesh)?;
    }
    if mesh.triangles.is_empty() {
        return Err(parse_error("scene contains no triangle primitives"));
    }
    Ok(mesh)
}

fn decode_buffer_uri(
    uri: &str,
    resource_dir: Option<&Path>,
) -> Result<Vec<u8>, ReconstructionError> {
    let Some(rest) = uri.strip_prefix("data:") else {
        return read_external_buffer(uri, resource_dir);
    };
    let Some((metadata, payload)) = rest.split_once(',') else {
        return Err(parse_error("malformed glTF data URI"));
    };
    if !metadata.ends_with(";base64") {
        return Err(parse_error(
            "only base64-encoded glTF data URI buffers are supported",
        ));
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|error| parse_error(format!("invalid glTF data URI: {error}")))
}

fn read_external_buffer(
    uri: &str,
    resource_dir: Option<&Path>,
) -> Result<Vec<u8>, ReconstructionError> {
    let base = resource_dir.ok_or_else(|| {
        parse_error(format!(
            "glTF buffer {uri:?} is external; use File > Open so VCAD can resolve it, or export GLB"
        ))
    })?;
    if uri.contains("://") || uri.starts_with('/') || uri.starts_with('\\') {
        return Err(parse_error(
            "remote and absolute glTF buffer URIs are unsupported",
        ));
    }
    let decoded = urlencoding::decode(uri)
        .map_err(|error| parse_error(format!("invalid glTF buffer URI: {error}")))?;
    let canonical_base = base
        .canonicalize()
        .map_err(|error| parse_error(format!("cannot access glTF directory: {error}")))?;
    let candidate = canonical_base.join(decoded.as_ref());
    let canonical_candidate = candidate
        .canonicalize()
        .map_err(|error| parse_error(format!("cannot read glTF buffer {uri:?}: {error}")))?;
    if !canonical_candidate.starts_with(&canonical_base) {
        return Err(parse_error(
            "glTF buffer URI escapes the selected file directory",
        ));
    }
    std::fs::read(&canonical_candidate)
        .map_err(|error| parse_error(format!("cannot read glTF buffer {uri:?}: {error}")))
}

fn append_node(
    node: gltf::Node<'_>,
    parent: [[f32; 4]; 4],
    buffers: &[Vec<u8>],
    output: &mut Mesh,
) -> Result<(), ReconstructionError> {
    let world = multiply(parent, node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if !matches!(
                primitive.mode(),
                Mode::Triangles | Mode::TriangleStrip | Mode::TriangleFan
            ) {
                continue;
            }
            let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
            let positions = reader
                .read_positions()
                .ok_or_else(|| parse_error("triangle primitive has no POSITION accessor"))?
                .collect::<Vec<_>>();
            let offset = output.vertices.len() as u32;
            output.vertices.extend(
                positions
                    .iter()
                    .copied()
                    .map(|position| transform_point(world, position)),
            );
            let indices = reader
                .read_indices()
                .map(|indices| indices.into_u32().collect::<Vec<_>>())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());
            append_primitive_indices(
                primitive.mode(),
                &indices,
                positions.len(),
                offset,
                determinant(world) < 0.0,
                &mut output.triangles,
            )?;
        }
    }
    for child in node.children() {
        append_node(child, world, buffers, output)?;
    }
    Ok(())
}

fn append_primitive_indices(
    mode: Mode,
    indices: &[u32],
    vertex_count: usize,
    offset: u32,
    reverse_winding: bool,
    output: &mut Vec<Triangle>,
) -> Result<(), ReconstructionError> {
    if let Some(index) = indices
        .iter()
        .copied()
        .find(|&index| index as usize >= vertex_count)
    {
        return Err(parse_error(format!(
            "primitive vertex index {index} is out of range"
        )));
    }
    let mut push = |a: u32, b: u32, c: u32| {
        if a != b && b != c && a != c {
            output.push(if reverse_winding {
                Triangle([offset + a, offset + c, offset + b])
            } else {
                Triangle([offset + a, offset + b, offset + c])
            });
        }
    };
    match mode {
        Mode::Triangles => {
            if !indices.len().is_multiple_of(3) {
                return Err(parse_error(
                    "triangle index count is not divisible by three",
                ));
            }
            for triangle in indices.chunks_exact(3) {
                push(triangle[0], triangle[1], triangle[2]);
            }
        }
        Mode::TriangleStrip => {
            for index in 2..indices.len() {
                if index % 2 == 0 {
                    push(indices[index - 2], indices[index - 1], indices[index]);
                } else {
                    push(indices[index - 1], indices[index - 2], indices[index]);
                }
            }
        }
        Mode::TriangleFan => {
            for index in 2..indices.len() {
                push(indices[0], indices[index - 1], indices[index]);
            }
        }
        _ => {}
    }
    Ok(())
}

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Multiply glTF column-major matrices (`parent * local`).
fn multiply(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut output = [[0.0; 4]; 4];
    for column in 0..4 {
        for row in 0..4 {
            output[column][row] = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    output
}

fn transform_point(matrix: [[f32; 4]; 4], point: [f32; 3]) -> [f64; 3] {
    let [x, y, z] = point;
    [
        (matrix[0][0] * x + matrix[1][0] * y + matrix[2][0] * z + matrix[3][0]) as f64,
        (matrix[0][1] * x + matrix[1][1] * y + matrix[2][1] * z + matrix[3][1]) as f64,
        (matrix[0][2] * x + matrix[1][2] * y + matrix[2][2] * z + matrix[3][2]) as f64,
    ]
}

fn determinant(matrix: [[f32; 4]; 4]) -> f32 {
    matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[2][1] * matrix[1][2])
        - matrix[1][0] * (matrix[0][1] * matrix[2][2] - matrix[2][1] * matrix[0][2])
        + matrix[2][0] * (matrix[0][1] * matrix[1][2] - matrix[1][1] * matrix[0][2])
}

fn parse_error(detail: impl Into<String>) -> ReconstructionError {
    ReconstructionError::Parse {
        format: "glTF",
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::parse_gltf;

    fn triangle_buffer() -> Vec<u8> {
        let mut buffer = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for index in [0_u16, 1, 2] {
            buffer.extend_from_slice(&index.to_le_bytes());
        }
        buffer.extend_from_slice(&[0, 0]);
        buffer
    }

    fn cube_buffer() -> Vec<u8> {
        let mut buffer = Vec::new();
        for point in [
            [0.0_f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ] {
            for value in point {
                buffer.extend_from_slice(&value.to_le_bytes());
            }
        }
        for index in [
            3_u16, 2, 1, 3, 1, 0, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2,
            7, 6, 3, 0, 4, 3, 4, 7,
        ] {
            buffer.extend_from_slice(&index.to_le_bytes());
        }
        buffer
    }

    fn document(buffer_uri: Option<&str>) -> String {
        let uri = buffer_uri
            .map(|uri| format!(r#", "uri": "{uri}""#))
            .unwrap_or_default();
        format!(
            r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":44{uri}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":6}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},{{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"mode":4}}]}}],"nodes":[{{"mesh":0,"translation":[2,3,4]}}],"scenes":[{{"nodes":[0]}}],"scene":0}}"#
        )
    }

    fn cube_document(uri: &str) -> String {
        format!(
            r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":168,"uri":"{uri}"}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":96}},{{"buffer":0,"byteOffset":96,"byteLength":72}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":8,"type":"VEC3","min":[0,0,0],"max":[1,1,1]}},{{"bufferView":1,"componentType":5123,"count":36,"type":"SCALAR"}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"mode":4}}]}}],"nodes":[{{"mesh":0}}],"scenes":[{{"nodes":[0]}}],"scene":0}}"#
        )
    }

    #[test]
    fn parses_embedded_gltf_and_applies_node_transform() {
        let buffer = triangle_buffer();
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(buffer)
        );
        let mesh = parse_gltf(document(Some(&uri)).as_bytes(), None).unwrap();
        assert_eq!(mesh.vertices[0], [2.0, 3.0, 4.0]);
        assert_eq!(mesh.triangles.len(), 1);
    }

    #[test]
    fn parses_glb_binary_chunk() {
        let mut json = document(None).into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let buffer = triangle_buffer();
        let total_length = 12 + 8 + json.len() + 8 + buffer.len();
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67_u32.to_le_bytes());
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&(total_length as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534A_u32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(buffer.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942_u32.to_le_bytes());
        glb.extend_from_slice(&buffer);

        let mesh = parse_gltf(&glb, None).unwrap();
        assert_eq!(mesh.vertices[1], [3.0, 3.0, 4.0]);
        assert_eq!(mesh.triangles.len(), 1);
    }

    #[test]
    fn resolves_external_buffer_beneath_selected_directory() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("triangle.bin"), triangle_buffer()).unwrap();
        let mesh = parse_gltf(
            document(Some("triangle.bin")).as_bytes(),
            Some(directory.path()),
        )
        .unwrap();
        assert_eq!(mesh.vertices[2], [2.0, 4.0, 4.0]);
        assert_eq!(mesh.triangles.len(), 1);
    }

    #[test]
    fn decodes_watertight_gltf_solid() {
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(cube_buffer())
        );
        let mesh = parse_gltf(cube_document(&uri).as_bytes(), None).unwrap();
        mesh.validate(0.001).unwrap();
        assert_eq!(mesh.triangles.len(), 12);
    }
}
