use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

/// GPU vertex layout shared by every importer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        points.into_iter().fold(Self::EMPTY, |b, p| b.including(p))
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    pub fn including(self, p: Vec3) -> Self {
        Aabb {
            min: self.min.min(p),
            max: self.max.max(p),
        }
    }

    pub fn union(self, other: Aabb) -> Self {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    pub fn radius(&self) -> f32 {
        self.size().length() * 0.5
    }

    /// Bounds of this box after an affine transform (Arvo's method).
    pub fn transformed(&self, m: &Mat4) -> Aabb {
        if self.is_empty() {
            return *self;
        }
        let translation = m.w_axis.truncate();
        let mut min = translation;
        let mut max = translation;
        for (i, axis) in [m.x_axis, m.y_axis, m.z_axis].iter().enumerate() {
            let a = axis.truncate() * self.min[i];
            let b = axis.truncate() * self.max[i];
            min += a.min(b);
            max += a.max(b);
        }
        Aabb { min, max }
    }

    /// Matrix mapping the unit cube [0, 1]^3 onto this box.
    pub fn unit_cube_transform(&self) -> Mat4 {
        Mat4::from_translation(self.min) * Mat4::from_scale(self.size().max(Vec3::splat(1e-6)))
    }
}

/// One drawable piece of geometry with a single material.
#[derive(Debug, Clone)]
pub struct Primitive {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub material: usize,
    pub bounds: Aabb,
    /// Counts survive after the CPU-side geometry is released post-upload.
    pub vertex_count: usize,
    pub index_count: usize,
}

impl Primitive {
    pub fn new(vertices: Vec<Vertex>, indices: Vec<u32>, material: usize) -> Self {
        let bounds = Aabb::from_points(vertices.iter().map(|v| Vec3::from(v.position)));
        Primitive {
            vertex_count: vertices.len(),
            index_count: indices.len(),
            vertices,
            indices,
            material,
            bounds,
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.index_count / 3
    }

    /// Release CPU geometry once it lives on the GPU.
    pub fn release_geometry(&mut self) {
        self.vertices = Vec::new();
        self.indices = Vec::new();
    }
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub name: String,
    pub primitives: Vec<Primitive>,
}

impl Mesh {
    pub fn bounds(&self) -> Aabb {
        self.primitives
            .iter()
            .fold(Aabb::EMPTY, |b, p| b.union(p.bounds))
    }
}

/// Smooth, area-weighted vertex normals for an indexed triangle list.
pub fn generate_normals(vertices: &mut [Vertex], indices: &[u32]) {
    let mut normals = vec![Vec3::ZERO; vertices.len()];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| i as usize);
        let pa = Vec3::from(vertices[a].position);
        let pb = Vec3::from(vertices[b].position);
        let pc = Vec3::from(vertices[c].position);
        // Unnormalized cross product weights by triangle area.
        let n = (pb - pa).cross(pc - pa);
        normals[a] += n;
        normals[b] += n;
        normals[c] += n;
    }
    for (v, n) in vertices.iter_mut().zip(normals) {
        v.normal = n.try_normalize().unwrap_or(Vec3::Y).into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transformed_bounds_cover_rotated_box() {
        let b = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        };
        let m = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))
            * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4);
        let t = b.transformed(&m);
        let r = 2f32.sqrt();
        assert!((t.min - Vec3::new(10.0 - r, -1.0, -r)).length() < 1e-5);
        assert!((t.max - Vec3::new(10.0 + r, 1.0, r)).length() < 1e-5);
    }

    #[test]
    fn empty_bounds_stay_empty() {
        assert!(Aabb::EMPTY.is_empty());
        assert!(Aabb::EMPTY.transformed(&Mat4::IDENTITY).is_empty());
        assert!(!Aabb::from_points([Vec3::ZERO]).is_empty());
    }

    #[test]
    fn generated_normals_follow_winding() {
        let mut vertices = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]].map(|p| Vertex {
            position: p,
            ..Default::default()
        });
        generate_normals(&mut vertices, &[0, 1, 2]);
        for v in vertices {
            assert_eq!(v.normal, [0.0, 0.0, 1.0]);
        }
    }
}
