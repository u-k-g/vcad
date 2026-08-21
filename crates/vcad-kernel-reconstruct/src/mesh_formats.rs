//! Triangle-container decoders that do not carry a scene graph.

use std::io::Cursor;

use ply_rs::parser::Parser;
use ply_rs::ply::{DefaultElement, Property};
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::{Mesh, ReconstructionError, Triangle};

pub(crate) fn parse_ply(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let parser = Parser::<DefaultElement>::new();
    let ply = parser
        .read_ply(&mut Cursor::new(data))
        .map_err(|error| parse_error("PLY", error))?;
    let vertex_elements = ply
        .payload
        .get("vertex")
        .ok_or_else(|| parse_detail("PLY", "missing vertex element"))?;
    let mut vertices = Vec::with_capacity(vertex_elements.len());
    for (index, vertex) in vertex_elements.iter().enumerate() {
        vertices.push([
            scalar(vertex.get("x")).ok_or_else(|| {
                parse_detail("PLY", format!("vertex {index} has no numeric x property"))
            })?,
            scalar(vertex.get("y")).ok_or_else(|| {
                parse_detail("PLY", format!("vertex {index} has no numeric y property"))
            })?,
            scalar(vertex.get("z")).ok_or_else(|| {
                parse_detail("PLY", format!("vertex {index} has no numeric z property"))
            })?,
        ]);
    }

    let face_elements = ply
        .payload
        .get("face")
        .ok_or_else(|| parse_detail("PLY", "missing face element"))?;
    let mut triangles = Vec::new();
    for (index, face) in face_elements.iter().enumerate() {
        let property = face
            .get("vertex_indices")
            .or_else(|| face.get("vertex_index"))
            .ok_or_else(|| parse_detail("PLY", format!("face {index} has no vertex index list")))?;
        let polygon = index_list(property)
            .ok_or_else(|| parse_detail("PLY", format!("face {index} indices are not integers")))?;
        triangulate_polygon(&polygon, vertices.len(), "PLY", index, &mut triangles)?;
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

fn scalar(property: Option<&Property>) -> Option<f64> {
    match property? {
        Property::Char(value) => Some(*value as f64),
        Property::UChar(value) => Some(*value as f64),
        Property::Short(value) => Some(*value as f64),
        Property::UShort(value) => Some(*value as f64),
        Property::Int(value) => Some(*value as f64),
        Property::UInt(value) => Some(*value as f64),
        Property::Float(value) => Some(*value as f64),
        Property::Double(value) => Some(*value),
        _ => None,
    }
}

fn index_list(property: &Property) -> Option<Vec<u32>> {
    macro_rules! signed {
        ($values:expr) => {
            $values
                .iter()
                .map(|&value| u32::try_from(value).ok())
                .collect::<Option<Vec<_>>>()
        };
    }
    Some(match property {
        Property::ListChar(values) => signed!(values)?,
        Property::ListUChar(values) => values.iter().map(|&value| value as u32).collect(),
        Property::ListShort(values) => signed!(values)?,
        Property::ListUShort(values) => values.iter().map(|&value| value as u32).collect(),
        Property::ListInt(values) => signed!(values)?,
        Property::ListUInt(values) => values.clone(),
        _ => return None,
    })
}

pub(crate) fn parse_off(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let text = std::str::from_utf8(data).map_err(|error| parse_error("OFF", error))?;
    let mut lines = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty());
    let header = lines
        .next()
        .ok_or_else(|| parse_detail("OFF", "file is empty"))?;
    let mut header_fields = header.split_whitespace();
    if header_fields.next() != Some("OFF") {
        return Err(parse_detail("OFF", "expected OFF header"));
    }
    let counts = if header_fields.clone().count() >= 3 {
        header_fields.collect::<Vec<_>>()
    } else {
        lines
            .next()
            .ok_or_else(|| parse_detail("OFF", "missing element counts"))?
            .split_whitespace()
            .collect::<Vec<_>>()
    };
    let vertex_count = parse_usize(counts.first().copied(), "OFF vertex count")?;
    let face_count = parse_usize(counts.get(1).copied(), "OFF face count")?;
    let mut vertices = Vec::with_capacity(vertex_count);
    for index in 0..vertex_count {
        let fields = lines
            .next()
            .ok_or_else(|| parse_detail("OFF", format!("missing vertex {index}")))?
            .split_whitespace()
            .take(3)
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| parse_error("OFF", error))?;
        if fields.len() != 3 {
            return Err(parse_detail(
                "OFF",
                format!("vertex {index} needs three coordinates"),
            ));
        }
        vertices.push([fields[0], fields[1], fields[2]]);
    }
    let mut triangles = Vec::new();
    for index in 0..face_count {
        let fields = lines
            .next()
            .ok_or_else(|| parse_detail("OFF", format!("missing face {index}")))?
            .split_whitespace()
            .collect::<Vec<_>>();
        let count = parse_usize(fields.first().copied(), "OFF face vertex count")?;
        if fields.len() < count + 1 {
            return Err(parse_detail(
                "OFF",
                format!("face {index} declares {count} vertices but provides fewer"),
            ));
        }
        let polygon = fields[1..=count]
            .iter()
            .map(|value| value.parse::<u32>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| parse_error("OFF", error))?;
        triangulate_polygon(&polygon, vertices.len(), "OFF", index, &mut triangles)?;
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

pub(crate) fn parse_amf(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let mut vertex = [None; 3];
    let mut triangle = [None; 3];
    let mut mesh_vertex_offset = 0_u32;
    let mut scale = 1.0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"amf" => {
                    for attribute in event.attributes() {
                        let attribute = attribute.map_err(|error| parse_error("AMF", error))?;
                        if attribute.key.local_name().as_ref() == b"unit" {
                            scale = amf_unit_scale(&String::from_utf8_lossy(&attribute.value))?;
                        }
                    }
                }
                b"mesh" => mesh_vertex_offset = vertices.len() as u32,
                b"vertex" => vertex = [None; 3],
                b"triangle" => triangle = [None; 3],
                b"x" => vertex[0] = Some(read_number(&mut reader, event.name(), "AMF")? * scale),
                b"y" => vertex[1] = Some(read_number(&mut reader, event.name(), "AMF")? * scale),
                b"z" => vertex[2] = Some(read_number(&mut reader, event.name(), "AMF")? * scale),
                b"v1" => triangle[0] = Some(read_index(&mut reader, event.name(), "AMF")?),
                b"v2" => triangle[1] = Some(read_index(&mut reader, event.name(), "AMF")?),
                b"v3" => triangle[2] = Some(read_index(&mut reader, event.name(), "AMF")?),
                _ => {}
            },
            Ok(Event::End(event)) if event.local_name().as_ref() == b"vertex" => {
                let [Some(x), Some(y), Some(z)] = vertex else {
                    return Err(parse_detail("AMF", "vertex is missing coordinates"));
                };
                vertices.push([x, y, z]);
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"triangle" => {
                let [Some(a), Some(b), Some(c)] = triangle else {
                    return Err(parse_detail("AMF", "triangle is missing indices"));
                };
                triangles.push(Triangle([
                    mesh_vertex_offset + a,
                    mesh_vertex_offset + b,
                    mesh_vertex_offset + c,
                ]));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(parse_error("AMF", error)),
        }
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

fn read_number(
    reader: &mut Reader<&[u8]>,
    end: quick_xml::name::QName<'_>,
    format: &'static str,
) -> Result<f64, ReconstructionError> {
    reader
        .read_text(end)
        .map_err(|error| parse_error(format, error))?
        .parse()
        .map_err(|error| parse_error(format, error))
}

fn read_index(
    reader: &mut Reader<&[u8]>,
    end: quick_xml::name::QName<'_>,
    format: &'static str,
) -> Result<u32, ReconstructionError> {
    reader
        .read_text(end)
        .map_err(|error| parse_error(format, error))?
        .parse()
        .map_err(|error| parse_error(format, error))
}

fn amf_unit_scale(unit: &str) -> Result<f64, ReconstructionError> {
    match unit.to_ascii_lowercase().as_str() {
        "millimeter" | "millimeters" => Ok(1.0),
        "micron" | "microns" => Ok(0.001),
        "meter" | "meters" => Ok(1000.0),
        "inch" | "inches" => Ok(25.4),
        "foot" | "feet" => Ok(304.8),
        other => Err(parse_detail("AMF", format!("unsupported unit {other}"))),
    }
}

fn triangulate_polygon(
    polygon: &[u32],
    vertex_count: usize,
    format: &'static str,
    face_index: usize,
    output: &mut Vec<Triangle>,
) -> Result<(), ReconstructionError> {
    if polygon.len() < 3 {
        return Err(parse_detail(
            format,
            format!("face {face_index} needs at least three vertices"),
        ));
    }
    if let Some(index) = polygon
        .iter()
        .copied()
        .find(|&index| index as usize >= vertex_count)
    {
        return Err(parse_detail(
            format,
            format!("face {face_index} vertex index {index} is out of range"),
        ));
    }
    for index in 1..polygon.len() - 1 {
        output.push(Triangle([polygon[0], polygon[index], polygon[index + 1]]));
    }
    Ok(())
}

fn parse_usize(value: Option<&str>, label: &str) -> Result<usize, ReconstructionError> {
    value
        .ok_or_else(|| parse_detail("OFF", format!("missing {label}")))?
        .parse()
        .map_err(|error| parse_error("OFF", error))
}

fn parse_error(format: &'static str, error: impl std::fmt::Display) -> ReconstructionError {
    ReconstructionError::Parse {
        format,
        detail: error.to_string(),
    }
}

fn parse_detail(format: &'static str, detail: impl Into<String>) -> ReconstructionError {
    ReconstructionError::Parse {
        format,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_amf, parse_off, parse_ply};

    #[test]
    fn parses_ascii_ply_polygon() {
        let mesh = parse_ply(
            b"ply\nformat ascii 1.0\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n",
        )
        .unwrap();
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.triangles.len(), 2);
    }

    #[test]
    fn parses_binary_little_endian_ply() {
        let mut data = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n".to_vec();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.push(3);
        for index in [0_i32, 1, 2] {
            data.extend_from_slice(&index.to_le_bytes());
        }
        let mesh = parse_ply(&data).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.triangles.len(), 1);
    }

    #[test]
    fn parses_off_polygon() {
        let mesh = parse_off(b"OFF\n4 1 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n").unwrap();
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.triangles.len(), 2);
    }

    #[test]
    fn parses_amf_mesh() {
        let mesh = parse_amf(br#"<?xml version="1.0"?><amf unit="inch"><object id="0"><mesh><vertices><vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex><vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex><vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex></vertices><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>"#).unwrap();
        assert_eq!(mesh.vertices[1], [25.4, 0.0, 0.0]);
        assert_eq!(mesh.triangles.len(), 1);
    }
}
