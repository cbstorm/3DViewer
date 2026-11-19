//! CPU ray picking against the scene's retained positions and indices.

use glam::Vec3;

use super::{mesh::Aabb, Primitive, Scene};

/// A world-space ray. `dir` need not be normalized; hit distances are in
/// units of `dir`.
#[derive(Debug, Clone, Copy)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub node: usize,
    /// Ray parameter of the hit: `origin + dir * t`.
    pub t: f32,
}

impl Ray {
    /// Entry parameter into `b`, if the ray hits it in front of the origin
    /// before `max_t`.
    fn hit_aabb(&self, b: &Aabb, max_t: f32) -> Option<f32> {
        if b.is_empty() {
            return None;
        }
        let inv = self.dir.recip();
        let t0 = (b.min - self.origin) * inv;
        let t1 = (b.max - self.origin) * inv;
        // NaN (0 * inf on a face-parallel ray) is dropped by min/max.
        let near = t0.min(t1).max_element().max(0.0);
        let far = t0.max(t1).min_element().min(max_t);
        (near <= far).then_some(near)
    }

    /// Möller–Trumbore, double-sided.
    fn hit_triangle(&self, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
        let e1 = b - a;
        let e2 = c - a;
        let p = self.dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < f32::MIN_POSITIVE {
            return None;
        }
        let inv_det = 1.0 / det;
        let s = self.origin - a;
        let u = s.dot(p) * inv_det;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let v = self.dir.dot(q) * inv_det;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = e2.dot(q) * inv_det;
        (t > 0.0).then_some(t)
    }

    fn hit_primitive(&self, p: &Primitive, max_t: f32) -> Option<f32> {
        self.hit_aabb(&p.bounds, max_t)?;
        let mut best = None;
        let mut max_t = max_t;
        for tri in p.indices.as_chunks::<3>().0 {
            let [Some(&a), Some(&b), Some(&c)] = tri.map(|i| p.positions.get(i as usize)) else {
                continue;
            };
            if let Some(t) = self.hit_triangle(a, b, c).filter(|&t| t < max_t) {
                max_t = t;
                best = Some(t);
            }
        }
        best
    }
}

impl Scene {
    /// Nearest visible node hit by `ray`.
    pub fn pick(&self, ray: Ray) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for (index, node) in self.nodes.iter().enumerate() {
            if !node.visible {
                continue;
            }
            let max_t = best.map_or(f32::INFINITY, |h| h.t);
            if ray.hit_aabb(&self.node_bounds(node), max_t).is_none() {
                continue;
            }
            let world = self.world_transform(node);
            if world.determinant().abs() < f32::MIN_POSITIVE {
                continue;
            }
            // Transform the ray rather than every vertex. Leaving the direction
            // unnormalized keeps `t` comparable across nodes.
            let inv = world.inverse();
            let local = Ray {
                origin: inv.transform_point3(ray.origin),
                dir: inv.transform_vector3(ray.dir),
            };
            for p in &self.meshes[node.mesh].primitives {
                let max_t = best.map_or(f32::INFINITY, |h| h.t);
                if let Some(t) = local.hit_primitive(p, max_t) {
                    best = Some(Hit { node: index, t });
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use glam::Mat4;

    use super::*;
    use crate::scene::{Mesh, Node, UpAxis, Vertex};

    /// A unit quad in the XY plane centered on the origin.
    fn quad() -> Mesh {
        let vertices = [
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ]
        .map(|p| Vertex {
            position: p,
            ..Default::default()
        });
        let mut p = Primitive::new(vertices.to_vec(), vec![0, 1, 2, 0, 2, 3], 0);
        p.release_geometry();
        Mesh {
            name: "quad".into(),
            primitives: vec![p],
        }
    }

    fn scene(nodes: &[(f32, bool)]) -> Scene {
        Scene {
            name: "test".into(),
            source: None,
            meshes: vec![quad()],
            materials: Vec::new(),
            textures: Vec::new(),
            nodes: nodes
                .iter()
                .map(|&(z, visible)| Node {
                    name: format!("z={z}"),
                    mesh: 0,
                    transform: Mat4::from_translation(Vec3::new(0.0, 0.0, z)),
                    depth: 0,
                    visible,
                })
                .collect(),
            up_axis: UpAxis::Y,
            load_time: Duration::ZERO,
        }
    }

    fn ray_from(x: f32, y: f32) -> Ray {
        Ray {
            origin: Vec3::new(x, y, 10.0),
            dir: Vec3::NEG_Z,
        }
    }

    #[test]
    fn picks_nearest_visible_node() {
        let s = scene(&[(-2.0, true), (1.0, true), (0.0, true)]);
        let hit = s.pick(ray_from(0.1, 0.2)).unwrap();
        assert_eq!(hit.node, 1);
        assert!((hit.t - 9.0).abs() < 1e-5);

        let s = scene(&[(-2.0, true), (1.0, false)]);
        assert_eq!(s.pick(ray_from(0.1, 0.2)).unwrap().node, 0);
    }

    #[test]
    fn misses_outside_and_behind() {
        let s = scene(&[(0.0, true)]);
        assert!(s.pick(ray_from(0.6, 0.0)).is_none());
        let away = Ray {
            dir: Vec3::Z,
            ..ray_from(0.0, 0.0)
        };
        assert!(s.pick(away).is_none());
    }

    #[test]
    fn respects_node_transform_and_up_axis() {
        let mut s = scene(&[(0.0, true)]);
        s.nodes[0].transform = Mat4::from_scale(Vec3::splat(4.0));
        assert!(s.pick(ray_from(1.5, 1.5)).is_some());
        // Z up rotates the quad's plane to XZ: a ray down -Z grazes its edge.
        s.up_axis = UpAxis::Z;
        assert!(s.pick(ray_from(1.5, 1.5)).is_none());
        let down = Ray {
            origin: Vec3::new(1.5, 10.0, 1.5),
            dir: Vec3::NEG_Y,
        };
        assert!(s.pick(down).is_some());
    }
}
