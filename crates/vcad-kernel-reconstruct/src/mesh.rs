use std::collections::{HashMap, HashSet, VecDeque};

use crate::ReconstructionError;

/// One indexed triangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Triangle(pub [u32; 3]);

/// Minimal f64 triangle mesh used by the native recognizer.
#[derive(Debug, Clone)]
pub struct Mesh {
    /// Welded vertex positions.
    pub vertices: Vec<[f64; 3]>,
    /// Consistently indexed triangles.
    pub triangles: Vec<Triangle>,
}

impl Mesh {
    /// Weld coincident vertices using the supplied absolute tolerance.
    pub fn welded(vertices: Vec<[f64; 3]>, triangles: Vec<Triangle>, tolerance: f64) -> Self {
        let cell = tolerance.max(1.0e-9);
        let mut buckets: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        let mut welded = Vec::<[f64; 3]>::new();
        let mut remap = Vec::with_capacity(vertices.len());

        for point in vertices {
            let key = [
                (point[0] / cell).round() as i64,
                (point[1] / cell).round() as i64,
                (point[2] / cell).round() as i64,
            ];
            let mut found = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let neighbor = [key[0] + dx, key[1] + dy, key[2] + dz];
                        for &candidate in buckets.get(&neighbor).into_iter().flatten() {
                            if distance(point, welded[candidate as usize]) <= tolerance {
                                found = Some(candidate);
                                break;
                            }
                        }
                    }
                }
            }
            let index = found.unwrap_or_else(|| {
                let index = welded.len() as u32;
                welded.push(point);
                buckets.entry(key).or_default().push(index);
                index
            });
            remap.push(index);
        }

        let mut triangles = triangles
            .into_iter()
            .map(|Triangle([a, b, c])| {
                Triangle([remap[a as usize], remap[b as usize], remap[c as usize]])
            })
            .collect::<Vec<_>>();
        let diagonal = bounding_diagonal(&welded).max(1.0);
        let area_epsilon = diagonal * diagonal * 1.0e-14;
        triangles.retain(|triangle| {
            let [a, b, c] = triangle.0;
            if a == b || b == c || c == a {
                return false;
            }
            let [a, b, c] = triangle.0.map(|index| welded[index as usize]);
            norm(cross(sub(b, a), sub(c, a))) > area_epsilon
        });
        Self {
            vertices: welded,
            triangles,
        }
    }

    /// Signed volume, made positive for consistently oriented closed meshes.
    pub fn volume(&self) -> f64 {
        self.triangles
            .iter()
            .map(|triangle| {
                let [a, b, c] = triangle.0.map(|index| self.vertices[index as usize]);
                dot(a, cross(b, c)) / 6.0
            })
            .sum::<f64>()
            .abs()
    }

    /// Unit normal and doubled area for a triangle.
    pub(crate) fn triangle_normal(&self, triangle: Triangle) -> ([f64; 3], f64) {
        let [a, b, c] = triangle.0.map(|index| self.vertices[index as usize]);
        let raw = cross(sub(b, a), sub(c, a));
        let length = norm(raw);
        if length == 0.0 {
            ([0.0; 3], 0.0)
        } else {
            ([raw[0] / length, raw[1] / length, raw[2] / length], length)
        }
    }

    /// Validate finiteness, manifold topology, connectedness, and volume.
    pub fn validate(&self, tolerance: f64) -> Result<(), ReconstructionError> {
        self.validate_impl(tolerance, true)
    }

    fn validate_impl(
        &self,
        tolerance: f64,
        require_triangle_manifold: bool,
    ) -> Result<(), ReconstructionError> {
        if self.vertices.len() < 4 || self.triangles.len() < 4 {
            return Err(ReconstructionError::InvalidMesh(
                "a solid requires at least four vertices and four triangles".into(),
            ));
        }
        if self
            .vertices
            .iter()
            .flatten()
            .any(|coordinate| !coordinate.is_finite())
        {
            return Err(ReconstructionError::InvalidMesh(
                "vertex coordinates must be finite".into(),
            ));
        }

        let area_epsilon = bounding_diagonal(&self.vertices).max(1.0).powi(2) * 1.0e-14;
        let mut edge_uses: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (triangle_index, triangle) in self.triangles.iter().copied().enumerate() {
            if triangle
                .0
                .iter()
                .any(|&index| index as usize >= self.vertices.len())
            {
                return Err(ReconstructionError::InvalidMesh(format!(
                    "triangle {triangle_index} references a missing vertex"
                )));
            }
            if triangle.0[0] == triangle.0[1]
                || triangle.0[1] == triangle.0[2]
                || triangle.0[2] == triangle.0[0]
                || self.triangle_normal(triangle).1 <= area_epsilon
            {
                return Err(ReconstructionError::InvalidMesh(format!(
                    "triangle {triangle_index} is degenerate"
                )));
            }
            for [a, b] in triangle_edges(triangle) {
                edge_uses
                    .entry(edge_key(a, b))
                    .or_default()
                    .push(triangle_index);
            }
        }

        if require_triangle_manifold {
            let boundary = edge_uses.values().filter(|uses| uses.len() == 1).count();
            let non_manifold = edge_uses.values().filter(|uses| uses.len() > 2).count();
            if boundary != 0 || non_manifold != 0 {
                return Err(ReconstructionError::InvalidMesh(format!(
                    "mesh is not a closed two-manifold ({boundary} boundary edges, {non_manifold} non-manifold edges)"
                )));
            }
        }

        let mut adjacency = vec![Vec::new(); self.triangles.len()];
        for uses in edge_uses.values() {
            if let [a, b] = uses.as_slice() {
                adjacency[*a].push(*b);
                adjacency[*b].push(*a);
            }
        }
        let mut reached = HashSet::new();
        let mut queue = VecDeque::from([0]);
        while let Some(current) = queue.pop_front() {
            if !reached.insert(current) {
                continue;
            }
            queue.extend(adjacency[current].iter().copied());
        }
        if require_triangle_manifold && reached.len() != self.triangles.len() {
            return Err(ReconstructionError::InvalidMesh(format!(
                "mesh contains {} disconnected shells; import each body separately",
                connected_components(&adjacency)
            )));
        }
        if self.volume() <= tolerance.powi(3) {
            return Err(ReconstructionError::InvalidMesh(
                "mesh encloses no measurable volume".into(),
            ));
        }
        Ok(())
    }
}

fn bounding_diagonal(vertices: &[[f64; 3]]) -> f64 {
    if vertices.is_empty() {
        return 0.0;
    }
    let mut low = vertices[0];
    let mut high = vertices[0];
    for point in &vertices[1..] {
        for axis in 0..3 {
            low[axis] = low[axis].min(point[axis]);
            high[axis] = high[axis].max(point[axis]);
        }
    }
    norm(sub(high, low))
}

pub(crate) fn triangle_edges(triangle: Triangle) -> [[u32; 2]; 3] {
    let [a, b, c] = triangle.0;
    [[a, b], [b, c], [c, a]]
}

pub(crate) fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn connected_components(adjacency: &[Vec<usize>]) -> usize {
    let mut unseen = (0..adjacency.len()).collect::<HashSet<_>>();
    let mut count = 0;
    while let Some(&start) = unseen.iter().next() {
        count += 1;
        let mut queue = VecDeque::from([start]);
        while let Some(current) = queue.pop_front() {
            if !unseen.remove(&current) {
                continue;
            }
            queue.extend(adjacency[current].iter().copied());
        }
    }
    count
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    norm(sub(a, b))
}

#[cfg(test)]
mod tests {
    use super::{Mesh, Triangle};

    fn tetrahedron() -> Mesh {
        Mesh::welded(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![
                Triangle([0, 2, 1]),
                Triangle([0, 1, 3]),
                Triangle([1, 2, 3]),
                Triangle([2, 0, 3]),
            ],
            1.0e-9,
        )
    }

    #[test]
    fn validates_closed_tetrahedron() {
        let mesh = tetrahedron();
        mesh.validate(1.0e-6).unwrap();
        assert!((mesh.volume() - 1.0 / 6.0).abs() < 1.0e-12);
    }

    #[test]
    fn refuses_open_mesh() {
        let mut mesh = tetrahedron();
        mesh.triangles.pop();
        assert!(mesh.validate(1.0e-6).is_err());
    }
}
