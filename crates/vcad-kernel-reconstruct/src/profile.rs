use std::collections::HashMap;

use geo::{Area, BooleanOps, Contains, Coord, LineString, MultiPolygon, Point, Polygon};

use crate::mesh::norm;
use crate::{
    Mesh, Reconstruction, ReconstructionError, ReconstructionOptions, ReconstructionReport,
};

const ANGULAR_TOLERANCE: f64 = 1.0e-3;

#[derive(Debug, Clone, Copy)]
struct AxisFrame {
    index: usize,
    name: &'static str,
    u: usize,
    v: usize,
}

const FRAMES: [AxisFrame; 3] = [
    AxisFrame {
        index: 0,
        name: "x",
        u: 1,
        v: 2,
    },
    AxisFrame {
        index: 1,
        name: "y",
        u: 0,
        v: 2,
    },
    AxisFrame {
        index: 2,
        name: "z",
        u: 0,
        v: 1,
    },
];

#[derive(Debug, Clone)]
struct Section {
    low: f64,
    high: f64,
    profile: MultiPolygon<f64>,
}

#[derive(Debug, Clone, Copy)]
enum ProfileSegment {
    Line {
        start: [f64; 2],
        end: [f64; 2],
    },
    Arc {
        start: [f64; 2],
        end: [f64; 2],
        center: [f64; 2],
        ccw: bool,
    },
}

#[derive(Default)]
struct Stats {
    output_segments: usize,
    arcs: usize,
    fillets: usize,
    chamfers: usize,
    max_deviation: f64,
    features: usize,
}

struct Emitter {
    lines: Vec<String>,
    counter: usize,
    options: ReconstructionOptions,
    frame: AxisFrame,
    stats: Stats,
}

pub(crate) fn reconstruct_feature_model(
    mesh: &Mesh,
    source_name: &str,
    options: ReconstructionOptions,
) -> Result<Reconstruction, ReconstructionError> {
    let frame = select_axis(mesh)?;
    let levels = feature_levels(mesh, frame, options.tolerance);
    if levels.len() < 2 {
        return Err(ReconstructionError::UnresolvedFeatures(
            "could not find opposing cap faces for a base extrusion".into(),
        ));
    }
    let sections = build_sections(mesh, frame, &levels, options.tolerance)?;
    if sections.is_empty() {
        return Err(ReconstructionError::UnresolvedFeatures(
            "no closed material profiles were found between feature faces".into(),
        ));
    }

    let reconstructed_volume = sections
        .iter()
        .map(|section| section.profile.unsigned_area() * (section.high - section.low))
        .sum::<f64>();
    let source_volume = mesh.volume();
    let relative_volume_error =
        (reconstructed_volume - source_volume).abs() / source_volume.max(1.0);

    let base_index = choose_base_profile(&sections, options.tolerance);
    let overall_low = sections.first().expect("non-empty sections").low;
    let overall_high = sections.last().expect("non-empty sections").high;
    let base = sections[base_index].profile.clone();

    let mut emitter = Emitter::new(frame, options, source_name);
    emitter.lines.push(format!(
        "; Base profile selected from feature interval {}..{} and extended across the body",
        emitter.fmt(sections[base_index].low),
        emitter.fmt(sections[base_index].high)
    ));
    let mut current = emitter.emit_multi_polygon(&base, overall_low, overall_high, "base")?;

    // The section profiles are evidence for feature inference only. We do not
    // publish one solid slab per interval. Instead, the chosen base spans the
    // entire body and each residual becomes a local additive or subtractive
    // extrusion, matching how a human-authored parametric history is built.
    for (index, section) in sections.iter().enumerate() {
        let additions = section.profile.difference(&base);
        if additions.unsigned_area() > options.tolerance * options.tolerance {
            emitter.lines.push(format!(
                "; Additive feature recovered from interval {}",
                index + 1
            ));
            let tool =
                emitter.emit_multi_polygon(&additions, section.low, section.high, "additive")?;
            current = emitter.boolean("union", &tool, &current);
        }

        let removals = base.difference(&section.profile);
        if removals.unsigned_area() > options.tolerance * options.tolerance {
            emitter.lines.push(format!(
                "; Subtractive feature recovered from interval {}",
                index + 1
            ));
            let tool =
                emitter.emit_multi_polygon(&removals, section.low, section.high, "subtractive")?;
            current = emitter.boolean("difference", &tool, &current);
        }
    }
    emitter.lines.push(String::new());
    emitter.lines.push(format!(
        "[root {current} \"default\"] ; One reconstructed body"
    ));

    let report = ReconstructionReport {
        axis: frame.name.into(),
        feature_count: emitter.stats.features,
        body_count: 1,
        source_volume,
        reconstructed_volume,
        relative_volume_error,
        source_triangles: mesh.triangles.len(),
        output_segments: emitter.stats.output_segments,
        recovered_arcs: emitter.stats.arcs,
        fillets: emitter.stats.fillets,
        chamfers: emitter.stats.chamfers,
        decimal_places: options.decimal_places,
        simplification_tolerance: options.tolerance,
        max_profile_deviation: emitter.stats.max_deviation,
    };
    Ok(Reconstruction {
        loon_source: emitter.lines.join("\n") + "\n",
        report,
    })
}

fn select_axis(mesh: &Mesh) -> Result<AxisFrame, ReconstructionError> {
    let mut candidates = Vec::new();
    for frame in FRAMES {
        let mut cap_area = 0.0;
        let mut side_area = 0.0;
        let mut unsupported_area = 0.0;
        for triangle in mesh.triangles.iter().copied() {
            let (normal, doubled_area) = mesh.triangle_normal(triangle);
            let component = normal[frame.index].abs();
            if 1.0 - component <= ANGULAR_TOLERANCE {
                cap_area += doubled_area;
            } else if component <= ANGULAR_TOLERANCE {
                side_area += doubled_area;
            } else {
                unsupported_area += doubled_area;
            }
        }
        if cap_area > 0.0 {
            candidates.push((
                unsupported_area / (cap_area + side_area + unsupported_area),
                frame,
            ));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some((unsupported_fraction, frame)) = candidates.first().copied() else {
        return Err(ReconstructionError::UnresolvedFeatures(
            "no dominant extrusion axis was found".into(),
        ));
    };
    if unsupported_fraction > 1.0e-5 {
        return Err(ReconstructionError::UnresolvedFeatures(format!(
            "{:.3}% of the surface varies continuously along every principal axis; native loft/revolve recognition is required before this model can be converted safely",
            unsupported_fraction * 100.0
        )));
    }
    Ok(frame)
}

fn feature_levels(mesh: &Mesh, frame: AxisFrame, tolerance: f64) -> Vec<f64> {
    let mut raw = Vec::new();
    for triangle in mesh.triangles.iter().copied() {
        let (normal, _) = mesh.triangle_normal(triangle);
        if 1.0 - normal[frame.index].abs() > ANGULAR_TOLERANCE {
            continue;
        }
        let depth = triangle
            .0
            .iter()
            .map(|&vertex| mesh.vertices[vertex as usize][frame.index])
            .sum::<f64>()
            / 3.0;
        raw.push(depth);
    }
    raw.sort_by(f64::total_cmp);
    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for value in raw {
        if clusters
            .last()
            .and_then(|cluster| cluster.last())
            .is_none_or(|previous| value - previous > tolerance)
        {
            clusters.push(vec![value]);
        } else {
            clusters.last_mut().expect("cluster exists").push(value);
        }
    }
    clusters
        .into_iter()
        .map(|cluster| cluster.iter().sum::<f64>() / cluster.len() as f64)
        .collect()
}

fn build_sections(
    mesh: &Mesh,
    frame: AxisFrame,
    levels: &[f64],
    tolerance: f64,
) -> Result<Vec<Section>, ReconstructionError> {
    let mut sections = Vec::new();
    for window in levels.windows(2) {
        let [low, high] = [window[0], window[1]];
        if high - low <= tolerance {
            continue;
        }
        let depth = (low + high) * 0.5;
        let loops = slice_loops(mesh, frame, depth, tolerance)?;
        if loops.is_empty() {
            continue;
        }
        let profile = loops_to_multi_polygon(loops)?;
        if profile.unsigned_area() > tolerance * tolerance {
            sections.push(Section { low, high, profile });
        }
    }
    Ok(sections)
}

fn slice_loops(
    mesh: &Mesh,
    frame: AxisFrame,
    depth: f64,
    tolerance: f64,
) -> Result<Vec<Vec<[f64; 2]>>, ReconstructionError> {
    let mut points = Vec::<[f64; 2]>::new();
    let mut buckets: HashMap<[i64; 2], Vec<usize>> = HashMap::new();
    let mut edges = Vec::<[usize; 2]>::new();
    for triangle in mesh.triangles.iter().copied() {
        let vertices = triangle.0.map(|index| mesh.vertices[index as usize]);
        let mut intersections = Vec::new();
        for edge in [[0, 1], [1, 2], [2, 0]] {
            let a = vertices[edge[0]];
            let b = vertices[edge[1]];
            let da = a[frame.index] - depth;
            let db = b[frame.index] - depth;
            if (da > 0.0) == (db > 0.0) || (da - db).abs() <= f64::EPSILON {
                continue;
            }
            let t = da / (da - db);
            intersections.push([
                a[frame.u] + (b[frame.u] - a[frame.u]) * t,
                a[frame.v] + (b[frame.v] - a[frame.v]) * t,
            ]);
        }
        deduplicate_points(&mut intersections, tolerance);
        if intersections.len() == 2 {
            let a = weld_point(intersections[0], tolerance, &mut points, &mut buckets);
            let b = weld_point(intersections[1], tolerance, &mut points, &mut buckets);
            if a != b {
                edges.push([a, b]);
            }
        }
    }

    let mut adjacency = vec![Vec::<usize>::new(); points.len()];
    for [a, b] in edges {
        if !adjacency[a].contains(&b) {
            adjacency[a].push(b);
            adjacency[b].push(a);
        }
    }
    if let Some((index, neighbors)) = adjacency
        .iter()
        .enumerate()
        .find(|(_, neighbors)| !neighbors.is_empty() && neighbors.len() != 2)
    {
        let nearest_open = adjacency
            .iter()
            .enumerate()
            .filter(|(candidate, candidate_neighbors)| {
                *candidate != index && candidate_neighbors.len() == 1
            })
            .map(|(candidate, _)| distance2(points[index], points[candidate]))
            .min_by(f64::total_cmp);
        return Err(ReconstructionError::UnresolvedFeatures(format!(
            "cross-section at {depth:.6} is not a collection of closed manifold loops (vertex {index} has degree {}; nearest open endpoint is {})",
            neighbors.len(),
            nearest_open.map_or_else(|| "unavailable".into(), |distance| format!("{distance:.6} units away"))
        )));
    }

    let mut unused = adjacency
        .iter()
        .enumerate()
        .filter(|(_, neighbors)| !neighbors.is_empty())
        .map(|(index, _)| index)
        .collect::<std::collections::HashSet<_>>();
    let mut loops = Vec::new();
    while let Some(&start) = unused.iter().next() {
        let mut ring = Vec::new();
        let mut previous = usize::MAX;
        let mut current = start;
        for _ in 0..=points.len() {
            ring.push(points[current]);
            unused.remove(&current);
            let next = adjacency[current]
                .iter()
                .copied()
                .find(|&candidate| candidate != previous)
                .expect("degree two loop");
            previous = current;
            current = next;
            if current == start {
                break;
            }
        }
        if current != start || ring.len() < 3 {
            return Err(ReconstructionError::UnresolvedFeatures(
                "cross-section chaining did not close".into(),
            ));
        }
        loops.push(ring);
    }
    Ok(loops)
}

fn deduplicate_points(points: &mut Vec<[f64; 2]>, tolerance: f64) {
    let mut unique = Vec::new();
    for point in points.drain(..) {
        if !unique
            .iter()
            .any(|other: &[f64; 2]| distance2(*other, point) <= tolerance)
        {
            unique.push(point);
        }
    }
    *points = unique;
}

fn weld_point(
    point: [f64; 2],
    tolerance: f64,
    points: &mut Vec<[f64; 2]>,
    buckets: &mut HashMap<[i64; 2], Vec<usize>>,
) -> usize {
    let cell = tolerance.max(1.0e-9);
    let key = [
        (point[0] / cell).round() as i64,
        (point[1] / cell).round() as i64,
    ];
    for dx in -1..=1 {
        for dy in -1..=1 {
            for &candidate in buckets
                .get(&[key[0] + dx, key[1] + dy])
                .into_iter()
                .flatten()
            {
                if distance2(points[candidate], point) <= tolerance {
                    return candidate;
                }
            }
        }
    }
    let index = points.len();
    points.push(point);
    buckets.entry(key).or_default().push(index);
    index
}

fn loops_to_multi_polygon(
    mut loops: Vec<Vec<[f64; 2]>>,
) -> Result<MultiPolygon<f64>, ReconstructionError> {
    loops.sort_by(|a, b| signed_area(b).abs().total_cmp(&signed_area(a).abs()));
    let rings = loops
        .iter()
        .map(|ring| {
            let mut coordinates = ring
                .iter()
                .map(|point| Coord {
                    x: point[0],
                    y: point[1],
                })
                .collect::<Vec<_>>();
            coordinates.push(coordinates[0]);
            LineString::new(coordinates)
        })
        .collect::<Vec<_>>();

    let mut depth = vec![0usize; rings.len()];
    for index in 0..rings.len() {
        let point = Point::from(rings[index].0[0]);
        depth[index] = (0..index)
            .filter(|&parent| Polygon::new(rings[parent].clone(), vec![]).contains(&point))
            .count();
    }
    let mut polygons = Vec::new();
    for index in 0..rings.len() {
        if depth[index] % 2 != 0 {
            continue;
        }
        let holes = (0..rings.len())
            .filter(|&hole| depth[hole] == depth[index] + 1)
            .filter(|&hole| {
                Polygon::new(rings[index].clone(), vec![]).contains(&Point::from(rings[hole].0[0]))
            })
            .map(|hole| rings[hole].clone())
            .collect();
        polygons.push(Polygon::new(rings[index].clone(), holes));
    }
    if polygons.is_empty() {
        return Err(ReconstructionError::UnresolvedFeatures(
            "cross-section contains no exterior profile".into(),
        ));
    }
    Ok(MultiPolygon(polygons))
}

fn choose_base_profile(sections: &[Section], tolerance: f64) -> usize {
    sections
        .iter()
        .enumerate()
        .map(|(candidate_index, candidate)| {
            let mut feature_cost = emitted_profile_cost(&candidate.profile);
            let mut correction_volume = 0.0;
            for section in sections {
                for delta in [
                    section.profile.difference(&candidate.profile),
                    candidate.profile.difference(&section.profile),
                ] {
                    if delta.unsigned_area() > tolerance * tolerance {
                        feature_cost += emitted_profile_cost(&delta) + 1;
                        correction_volume += delta.unsigned_area() * (section.high - section.low);
                    }
                }
            }
            (feature_cost, correction_volume, candidate_index)
        })
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)))
        .map(|(_, _, index)| index)
        .unwrap_or(0)
}

fn emitted_profile_cost(profile: &MultiPolygon<f64>) -> usize {
    let polygons = profile.0.len();
    if polygons == 0 {
        return 0;
    }
    let holes = profile
        .0
        .iter()
        .map(|polygon| polygon.interiors().len())
        .sum::<usize>();
    // One extrusion per loop, one difference per hole, and one union between
    // disconnected exterior components.
    polygons + holes * 2 + polygons.saturating_sub(1)
}

impl Emitter {
    fn new(frame: AxisFrame, options: ReconstructionOptions, source_name: &str) -> Self {
        Self {
            lines: vec![
                "; Reconstructed as native VCAD feature geometry".into(),
                format!("; Source: {}", source_name.replace(['\n', '\r'], " ")),
                "; The source file is not embedded or referenced by this document.".into(),
                "; Reconstruction strategy: one spanning base extrusion plus local additive/subtractive features.".into(),
                String::new(),
            ],
            counter: 0,
            options,
            frame,
            stats: Stats::default(),
        }
    }

    fn emit_multi_polygon(
        &mut self,
        profile: &MultiPolygon<f64>,
        low: f64,
        high: f64,
        label: &str,
    ) -> Result<String, ReconstructionError> {
        let mut current: Option<String> = None;
        for polygon in &profile.0 {
            if polygon.unsigned_area() <= self.options.tolerance * self.options.tolerance {
                continue;
            }
            let body = self.emit_polygon(polygon, low, high, label)?;
            current = Some(match current {
                Some(subject) => self.boolean("union", &body, &subject),
                None => body,
            });
        }
        let Some(current) = current else {
            return Err(ReconstructionError::UnresolvedFeatures(format!(
                "{label} feature collapsed below the requested tolerance"
            )));
        };
        Ok(current)
    }

    fn emit_polygon(
        &mut self,
        polygon: &Polygon<f64>,
        low: f64,
        high: f64,
        label: &str,
    ) -> Result<String, ReconstructionError> {
        let exterior = ring_points(polygon.exterior());
        let mut current = self.emit_extrusion(&exterior, low, high, label)?;
        for hole in polygon.interiors() {
            let hole_points = ring_points(hole);
            let tool = self.emit_extrusion(&hole_points, low, high, "hole")?;
            current = self.boolean("difference", &tool, &current);
        }
        Ok(current)
    }

    fn emit_extrusion(
        &mut self,
        points: &[[f64; 2]],
        low: f64,
        high: f64,
        label: &str,
    ) -> Result<String, ReconstructionError> {
        let (segments, deviation) = recover_profile_segments(points, self.options.tolerance);
        self.stats.max_deviation = self.stats.max_deviation.max(deviation);
        self.stats.output_segments += segments.len();
        self.stats.arcs += segments
            .iter()
            .filter(|segment| matches!(segment, ProfileSegment::Arc { .. }))
            .count();
        self.stats.fillets += recognized_fillet_count(&segments, self.options.tolerance);
        self.stats.chamfers += recognized_chamfer_count(&segments, self.options.tolerance);

        self.counter += 1;
        let sketch = format!("{label}-profile-{}", self.counter);
        let body = format!("{label}-extrusion-{}", self.counter);
        let origin = axis_vector(self.frame.index, low);
        let u = axis_vector(self.frame.u, 1.0);
        let v = axis_vector(self.frame.v, 1.0);
        self.lines.push(format!("[let {sketch} [sketch"));
        self.lines.push(format!(
            "  {} {} {}",
            self.fmt(origin[0]),
            self.fmt(origin[1]),
            self.fmt(origin[2])
        ));
        self.lines.push(format!(
            "  {} {} {}",
            self.fmt(u[0]),
            self.fmt(u[1]),
            self.fmt(u[2])
        ));
        self.lines.push(format!(
            "  {} {} {}",
            self.fmt(v[0]),
            self.fmt(v[1]),
            self.fmt(v[2])
        ));
        self.lines.push("  #[".into());
        for segment in segments {
            match segment {
                ProfileSegment::Line { start, end } => self.lines.push(format!(
                    "    [line {} {} {} {}]",
                    self.fmt(start[0]),
                    self.fmt(start[1]),
                    self.fmt(end[0]),
                    self.fmt(end[1])
                )),
                ProfileSegment::Arc {
                    start,
                    end,
                    center,
                    ccw,
                } => self.lines.push(format!(
                    "    [arc {} {} {} {} {} {} {}]",
                    self.fmt(start[0]),
                    self.fmt(start[1]),
                    self.fmt(end[0]),
                    self.fmt(end[1]),
                    self.fmt(center[0]),
                    self.fmt(center[1]),
                    ccw
                )),
            }
        }
        self.lines.push("  ]]]".into());
        let direction = axis_vector(self.frame.index, high - low);
        self.lines.push(format!(
            "[let {body} [extrude {} {} {} {sketch}]]",
            self.fmt(direction[0]),
            self.fmt(direction[1]),
            self.fmt(direction[2])
        ));
        self.stats.features += 1;
        Ok(body)
    }

    fn boolean(&mut self, operation: &str, tool: &str, subject: &str) -> String {
        self.counter += 1;
        let name = format!("reconstructed-{operation}-{}", self.counter);
        self.lines
            .push(format!("[let {name} [{operation} {tool} {subject}]]"));
        self.stats.features += 1;
        name
    }

    fn fmt(&self, mut value: f64) -> String {
        let zero = if self.options.decimal_places == 0 {
            0.5
        } else {
            0.5 * 10.0f64.powi(-(self.options.decimal_places as i32))
        };
        if value.abs() < zero {
            value = 0.0;
        }
        let mut text = format!("{:.*}", self.options.decimal_places as usize, value);
        if text.contains('.') {
            text = text.trim_end_matches('0').trim_end_matches('.').into();
        }
        if text == "-0" {
            "0".into()
        } else {
            text
        }
    }
}

fn ring_points(ring: &LineString<f64>) -> Vec<[f64; 2]> {
    let mut points = ring
        .0
        .iter()
        .map(|coordinate| [coordinate.x, coordinate.y])
        .collect::<Vec<_>>();
    if points.len() > 1 && distance2(points[0], *points.last().expect("point")) <= 1.0e-10 {
        points.pop();
    }
    points
}

fn recover_profile_segments(points: &[[f64; 2]], tolerance: f64) -> (Vec<ProfileSegment>, f64) {
    if points.len() < 3 {
        return (Vec::new(), 0.0);
    }
    if let Some((center, _radius, error)) = fit_circle(points, true) {
        if points.len() >= 8 && error <= tolerance {
            let half = points.len() / 2;
            let ccw = signed_area(points) > 0.0;
            return (
                vec![
                    ProfileSegment::Arc {
                        start: points[0],
                        end: points[half],
                        center,
                        ccw,
                    },
                    ProfileSegment::Arc {
                        start: points[half],
                        end: points[0],
                        center,
                        ccw,
                    },
                ],
                error,
            );
        }
    }

    let count = points.len();
    let turns = (0..count)
        .map(|index| {
            signed_turn(
                points[(index + count - 1) % count],
                points[index],
                points[(index + 1) % count],
            )
        })
        .collect::<Vec<_>>();
    let rotation = (0..count)
        .min_by(|&a, &b| turns[a].abs().total_cmp(&turns[b].abs()))
        .unwrap_or(0);
    let rotated = (0..count)
        .map(|offset| points[(rotation + offset) % count])
        .collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut max_deviation: f64 = 0.0;
    let mut index = 0;
    while index < count {
        let mut best = None;
        for end in (index + 4..=count).rev() {
            let run = &rotated[index..end];
            let total_turn = run
                .windows(3)
                .map(|window| signed_turn(window[0], window[1], window[2]))
                .sum::<f64>();
            let consistent = run.windows(3).all(|window| {
                let turn = signed_turn(window[0], window[1], window[2]);
                turn.abs() <= 0.4 && turn.signum() == total_turn.signum()
            });
            if !consistent || total_turn.abs() < 0.12 {
                continue;
            }
            if let Some((center, _radius, error)) = fit_circle(run, false) {
                if error <= tolerance {
                    best = Some((end, center, total_turn > 0.0, error));
                    break;
                }
            }
        }
        if let Some((end, center, ccw, error)) = best {
            segments.push(ProfileSegment::Arc {
                start: rotated[index],
                end: rotated[end - 1],
                center,
                ccw,
            });
            max_deviation = max_deviation.max(error);
            index = end - 1;
        } else {
            let end = rotated[(index + 1) % count];
            segments.push(ProfileSegment::Line {
                start: rotated[index],
                end,
            });
            index += 1;
        }
    }
    (merge_collinear(segments, tolerance), max_deviation)
}

fn merge_collinear(segments: Vec<ProfileSegment>, tolerance: f64) -> Vec<ProfileSegment> {
    let mut out = Vec::new();
    for segment in segments {
        match (out.last_mut(), segment) {
            (
                Some(ProfileSegment::Line {
                    start,
                    end: previous_end,
                }),
                ProfileSegment::Line {
                    start: next_start,
                    end,
                },
            ) if distance2(*previous_end, next_start) <= tolerance => {
                let chord = [end[0] - start[0], end[1] - start[1]];
                let length = distance2(*start, end);
                let deviation = if length > 0.0 {
                    (chord[0] * (previous_end[1] - start[1])
                        - chord[1] * (previous_end[0] - start[0]))
                        .abs()
                        / length
                } else {
                    f64::INFINITY
                };
                if deviation <= tolerance {
                    *previous_end = end;
                } else {
                    out.push(ProfileSegment::Line {
                        start: next_start,
                        end,
                    });
                }
            }
            (_, segment) => out.push(segment),
        }
    }
    out
}

fn fit_circle(points: &[[f64; 2]], closed: bool) -> Option<([f64; 2], f64, f64)> {
    if points.len() < 3 {
        return None;
    }
    let a = points[0];
    let b = points[points.len() / 2];
    let c = if closed {
        points[points.len() / 3]
    } else {
        points[points.len() - 1]
    };
    let center = circle_from_three(a, b, c).or_else(|| {
        circle_from_three(
            points[0],
            points[points.len() / 3],
            points[2 * points.len() / 3],
        )
    })?;
    let radii = points
        .iter()
        .map(|&point| distance2(point, center))
        .collect::<Vec<_>>();
    let radius = radii.iter().sum::<f64>() / radii.len() as f64;
    if radius <= 1.0e-12 {
        return None;
    }
    let mut error = radii
        .iter()
        .map(|value| (value - radius).abs())
        .fold(0.0, f64::max);
    let pairs = points.windows(2).map(|window| (window[0], window[1]));
    for (start, end) in pairs.chain(closed.then(|| (points[points.len() - 1], points[0]))) {
        let midpoint = [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5];
        error = error.max((distance2(midpoint, center) - radius).abs());
    }
    Some((center, radius, error))
}

fn circle_from_three(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> Option<[f64; 2]> {
    let determinant = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    if determinant.abs() <= 1.0e-12 {
        return None;
    }
    let a2 = a[0] * a[0] + a[1] * a[1];
    let b2 = b[0] * b[0] + b[1] * b[1];
    let c2 = c[0] * c[0] + c[1] * c[1];
    Some([
        (a2 * (b[1] - c[1]) + b2 * (c[1] - a[1]) + c2 * (a[1] - b[1])) / determinant,
        (a2 * (c[0] - b[0]) + b2 * (a[0] - c[0]) + c2 * (b[0] - a[0])) / determinant,
    ])
}

fn recognized_fillet_count(segments: &[ProfileSegment], tolerance: f64) -> usize {
    segments
        .iter()
        .enumerate()
        .filter(|(index, segment)| {
            let ProfileSegment::Arc {
                start, end, center, ..
            } = segment
            else {
                return false;
            };
            let previous = &segments[(*index + segments.len() - 1) % segments.len()];
            let next = &segments[(*index + 1) % segments.len()];
            tangent_to(previous, *start, *center, tolerance)
                && tangent_to(next, *end, *center, tolerance)
        })
        .count()
}

fn tangent_to(
    segment: &ProfileSegment,
    contact: [f64; 2],
    center: [f64; 2],
    tolerance: f64,
) -> bool {
    let ProfileSegment::Line { start, end } = segment else {
        return false;
    };
    let line = [end[0] - start[0], end[1] - start[1]];
    let radius = [contact[0] - center[0], contact[1] - center[1]];
    let denominator = distance2(*start, *end) * distance2(contact, center);
    denominator > tolerance
        && (line[0] * radius[0] + line[1] * radius[1]).abs() / denominator < 0.02
}

fn recognized_chamfer_count(segments: &[ProfileSegment], tolerance: f64) -> usize {
    if segments.len() < 3 {
        return 0;
    }
    segments
        .iter()
        .enumerate()
        .filter(|(index, segment)| {
            let ProfileSegment::Line { start, end } = segment else {
                return false;
            };
            let previous = &segments[(*index + segments.len() - 1) % segments.len()];
            let next = &segments[(*index + 1) % segments.len()];
            let (
                ProfileSegment::Line { start: pa, end: pb },
                ProfileSegment::Line { start: na, end: nb },
            ) = (previous, next)
            else {
                return false;
            };
            let Some(corner) = line_intersection(*pa, *pb, *na, *nb) else {
                return false;
            };
            let setback_a = distance2(corner, *start);
            let setback_b = distance2(corner, *end);
            let neighbor_scale = distance2(*pa, *pb).min(distance2(*na, *nb));
            let turn_a = signed_turn(*pa, *start, *end);
            let turn_b = signed_turn(*start, *end, *nb);
            setback_a > tolerance
                && setback_b > tolerance
                && setback_a.max(setback_b) <= neighbor_scale
                && (setback_a - setback_b).abs() <= tolerance.max(setback_a.max(setback_b) * 0.03)
                && turn_a.abs() > 0.15
                && turn_b.abs() > 0.15
                && turn_a.signum() == turn_b.signum()
        })
        .count()
}

fn line_intersection(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> Option<[f64; 2]> {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let cd = [d[0] - c[0], d[1] - c[1]];
    let denominator = ab[0] * cd[1] - ab[1] * cd[0];
    if denominator.abs() <= 1.0e-12 {
        return None;
    }
    let ac = [c[0] - a[0], c[1] - a[1]];
    let t = (ac[0] * cd[1] - ac[1] * cd[0]) / denominator;
    Some([a[0] + ab[0] * t, a[1] + ab[1] * t])
}

fn axis_vector(index: usize, value: f64) -> [f64; 3] {
    let mut vector = [0.0; 3];
    vector[index] = value;
    vector
}

fn distance2(a: [f64; 2], b: [f64; 2]) -> f64 {
    norm([a[0] - b[0], a[1] - b[1], 0.0])
}

fn signed_area(points: &[[f64; 2]]) -> f64 {
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let next = points[(index + 1) % points.len()];
            point[0] * next[1] - next[0] * point[1]
        })
        .sum::<f64>()
        * 0.5
}

fn signed_turn(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    let incoming = [b[0] - a[0], b[1] - a[1]];
    let outgoing = [c[0] - b[0], c[1] - b[1]];
    (incoming[0] * outgoing[1] - incoming[1] * outgoing[0])
        .atan2(incoming[0] * outgoing[0] + incoming[1] * outgoing[1])
}

#[cfg(test)]
mod tests {
    use super::{recover_profile_segments, ProfileSegment};

    #[test]
    fn recovers_tessellated_circle_as_two_arcs() {
        let points = (0..64)
            .map(|index| {
                let angle = index as f64 / 64.0 * std::f64::consts::TAU;
                [3.0 + 2.0 * angle.cos(), -4.0 + 2.0 * angle.sin()]
            })
            .collect::<Vec<_>>();
        let (segments, deviation) = recover_profile_segments(&points, 0.01);
        assert_eq!(segments.len(), 2);
        assert!(segments
            .iter()
            .all(|segment| matches!(segment, ProfileSegment::Arc { .. })));
        assert!(deviation < 0.01);
    }
}
