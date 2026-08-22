use std::io::{BufReader, Cursor, Read};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

use crate::{Mesh, ReconstructionError, SourceFormat, Triangle};

pub(crate) fn parse(
    data: &[u8],
    format: SourceFormat,
    resource_dir: Option<&Path>,
) -> Result<Mesh, ReconstructionError> {
    let mesh = match format {
        SourceFormat::Step => crate::occ::parse_step(data)?,
        SourceFormat::Stl => parse_stl(data)?,
        SourceFormat::Obj => parse_obj(data)?,
        SourceFormat::ThreeMf => parse_three_mf(data)?,
        SourceFormat::Ply => crate::mesh_formats::parse_ply(data)?,
        SourceFormat::Gltf => crate::scene_formats::parse_gltf(data, resource_dir)?,
        SourceFormat::Off => crate::mesh_formats::parse_off(data)?,
        SourceFormat::Amf => crate::mesh_formats::parse_amf(data)?,
    };
    // STEP faces are tessellated independently and meet with small f32 seam
    // noise. A 0.1 micron weld closes those representational seams while
    // remaining far below the reconstruction tolerance exposed to users.
    Ok(Mesh::welded(mesh.vertices, mesh.triangles, 1.0e-4))
}

fn parse_stl(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let stl = stl_io::read_stl(&mut BufReader::new(Cursor::new(data))).map_err(|error| {
        ReconstructionError::Parse {
            format: "STL",
            detail: error.to_string(),
        }
    })?;
    Ok(Mesh {
        vertices: stl
            .vertices
            .iter()
            .map(|point| [point[0] as f64, point[1] as f64, point[2] as f64])
            .collect(),
        triangles: stl
            .faces
            .iter()
            .map(|face| {
                Triangle([
                    face.vertices[0] as u32,
                    face.vertices[1] as u32,
                    face.vertices[2] as u32,
                ])
            })
            .collect(),
    })
}

fn parse_obj(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let text = std::str::from_utf8(data).map_err(|error| ReconstructionError::Parse {
        format: "OBJ",
        detail: error.to_string(),
    })?;
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("v") => {
                let coordinates = fields
                    .take(3)
                    .map(str::parse::<f64>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| ReconstructionError::Parse {
                        format: "OBJ",
                        detail: format!("line {}: {error}", line_number + 1),
                    })?;
                if coordinates.len() != 3 {
                    return Err(ReconstructionError::Parse {
                        format: "OBJ",
                        detail: format!("line {}: vertex needs three coordinates", line_number + 1),
                    });
                }
                vertices.push([coordinates[0], coordinates[1], coordinates[2]]);
            }
            Some("f") => {
                let polygon = fields
                    .map(|field| parse_obj_index(field, vertices.len(), line_number + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                if polygon.len() < 3 {
                    return Err(ReconstructionError::Parse {
                        format: "OBJ",
                        detail: format!(
                            "line {}: face needs at least three vertices",
                            line_number + 1
                        ),
                    });
                }
                for index in 1..polygon.len() - 1 {
                    triangles.push(Triangle([polygon[0], polygon[index], polygon[index + 1]]));
                }
            }
            _ => {}
        }
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

fn parse_obj_index(
    field: &str,
    vertex_count: usize,
    line_number: usize,
) -> Result<u32, ReconstructionError> {
    let value = field
        .split('/')
        .next()
        .unwrap_or_default()
        .parse::<isize>()
        .map_err(|error| ReconstructionError::Parse {
            format: "OBJ",
            detail: format!("line {line_number}: {error}"),
        })?;
    let zero_based = if value > 0 {
        value - 1
    } else if value < 0 {
        vertex_count as isize + value
    } else {
        -1
    };
    if zero_based < 0 || zero_based as usize >= vertex_count {
        return Err(ReconstructionError::Parse {
            format: "OBJ",
            detail: format!("line {line_number}: vertex index {value} is out of range"),
        });
    }
    Ok(zero_based as u32)
}

fn parse_three_mf(data: &[u8]) -> Result<Mesh, ReconstructionError> {
    let mut archive =
        ZipArchive::new(Cursor::new(data)).map_err(|error| ReconstructionError::Parse {
            format: "3MF",
            detail: error.to_string(),
        })?;
    let model_name = (0..archive.len())
        .find_map(|index| {
            let name = archive.by_index(index).ok()?.name().to_string();
            name.to_ascii_lowercase()
                .ends_with(".model")
                .then_some(name)
        })
        .ok_or_else(|| ReconstructionError::Parse {
            format: "3MF",
            detail: "container has no 3D model XML".into(),
        })?;
    let mut xml = Vec::new();
    let mut model_file =
        archive
            .by_name(&model_name)
            .map_err(|error| ReconstructionError::Parse {
                format: "3MF",
                detail: error.to_string(),
            })?;
    model_file
        .read_to_end(&mut xml)
        .map_err(|error| ReconstructionError::Parse {
            format: "3MF",
            detail: error.to_string(),
        })?;

    let mut reader = Reader::from_reader(xml.as_slice());
    reader.config_mut().trim_text(true);
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let mut in_vertices = false;
    let mut in_triangles = false;
    let mut scale = 1.0;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"model" => {
                    if let Some(unit) = attribute(&event, b"unit")? {
                        scale = unit_scale(&unit)?;
                    }
                }
                b"vertices" => in_vertices = true,
                b"triangles" => in_triangles = true,
                _ => {}
            },
            Ok(Event::Empty(event)) if in_vertices && event.local_name().as_ref() == b"vertex" => {
                vertices.push([
                    number_attribute(&event, b"x")? * scale,
                    number_attribute(&event, b"y")? * scale,
                    number_attribute(&event, b"z")? * scale,
                ]);
            }
            Ok(Event::Empty(event))
                if in_triangles && event.local_name().as_ref() == b"triangle" =>
            {
                triangles.push(Triangle([
                    integer_attribute(&event, b"v1")?,
                    integer_attribute(&event, b"v2")?,
                    integer_attribute(&event, b"v3")?,
                ]));
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"vertices" => in_vertices = false,
                b"triangles" => in_triangles = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(ReconstructionError::Parse {
                    format: "3MF",
                    detail: error.to_string(),
                })
            }
        }
    }
    if vertices.is_empty() || triangles.is_empty() {
        return Err(ReconstructionError::Parse {
            format: "3MF",
            detail: "model contains no directly embedded triangle mesh".into(),
        });
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

fn attribute(
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, ReconstructionError> {
    for attribute in event.attributes() {
        let attribute = attribute.map_err(|error| ReconstructionError::Parse {
            format: "3MF",
            detail: error.to_string(),
        })?;
        if attribute.key.local_name().as_ref() == name {
            return Ok(Some(
                String::from_utf8_lossy(attribute.value.as_ref()).into_owned(),
            ));
        }
    }
    Ok(None)
}

fn number_attribute(
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<f64, ReconstructionError> {
    let value = attribute(event, name)?.ok_or_else(|| ReconstructionError::Parse {
        format: "3MF",
        detail: format!("missing {} attribute", String::from_utf8_lossy(name)),
    })?;
    value.parse().map_err(|error| ReconstructionError::Parse {
        format: "3MF",
        detail: format!(
            "invalid {} attribute: {error}",
            String::from_utf8_lossy(name)
        ),
    })
}

fn integer_attribute(
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<u32, ReconstructionError> {
    let value = attribute(event, name)?.ok_or_else(|| ReconstructionError::Parse {
        format: "3MF",
        detail: format!("missing {} attribute", String::from_utf8_lossy(name)),
    })?;
    value.parse().map_err(|error| ReconstructionError::Parse {
        format: "3MF",
        detail: format!(
            "invalid {} attribute: {error}",
            String::from_utf8_lossy(name)
        ),
    })
}

fn unit_scale(unit: &str) -> Result<f64, ReconstructionError> {
    match unit {
        "micron" => Ok(0.001),
        "millimeter" => Ok(1.0),
        "centimeter" => Ok(10.0),
        "meter" => Ok(1000.0),
        "inch" => Ok(25.4),
        "foot" => Ok(304.8),
        other => Err(ReconstructionError::Parse {
            format: "3MF",
            detail: format!("unsupported model unit {other}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_obj;

    #[test]
    fn parses_negative_obj_indices_and_triangulates_polygons() {
        let mesh = parse_obj(b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf -4 -3 -2 -1\n").unwrap();
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.triangles.len(), 2);
    }
}
