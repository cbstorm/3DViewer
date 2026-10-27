//! Built-in scene shown at startup: two rows of spheres sweeping roughness
//! (dielectric, then metal), plus a cube.

use std::{f32::consts::PI, time::Duration};

use glam::{Mat4, Vec3};

use super::{
    mesh::{generate_normals, Mesh, Primitive, Vertex},
    scene::{Material, Node, Scene, UpAxis},
};

pub fn demo_scene() -> Scene {
    let mut materials = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    const COLUMNS: usize = 5;

    for row in 0..2 {
        for col in 0..COLUMNS {
            let metallic = row as f32;
            let roughness = 0.1 + 0.85 * col as f32 / (COLUMNS - 1) as f32;
            materials.push(Material {
                name: format!("metallic {metallic:.1} / roughness {roughness:.2}"),
                base_color: if row == 0 {
                    [0.8, 0.12, 0.1, 1.0]
                } else {
                    [0.95, 0.75, 0.35, 1.0]
                },
                metallic,
                roughness,
                ..Default::default()
            });
            // Materials belong to primitives, so each sphere needs its own mesh.
            let name = format!("Sphere {}", nodes.len() + 1);
            meshes.push(Mesh {
                name: name.clone(),
                primitives: vec![uv_sphere(0.5, 48, 32, materials.len() - 1)],
            });
            nodes.push(Node {
                name,
                mesh: meshes.len() - 1,
                transform: Mat4::from_translation(Vec3::new(
                    (col as f32 - (COLUMNS - 1) as f32 * 0.5) * 1.25,
                    1.0 + row as f32 * 1.25,
                    0.0,
                )),
                depth: 0,
                visible: true,
            });
        }
    }

    materials.push(Material {
        name: "Cube".into(),
        base_color: [0.25, 0.45, 0.8, 1.0],
        roughness: 0.45,
        ..Default::default()
    });
    meshes.push(Mesh {
        name: "Cube".into(),
        primitives: vec![cube(materials.len() - 1)],
    });
    nodes.push(Node {
        name: "Cube".into(),
        mesh: meshes.len() - 1,
        transform: Mat4::from_translation(Vec3::new(0.0, 0.3, 1.6))
            * Mat4::from_rotation_y(0.5)
            * Mat4::from_scale(Vec3::splat(0.6)),
        depth: 0,
        visible: true,
    });

    Scene {
        name: "Demo scene".into(),
        source: None,
        meshes,
        materials,
        textures: Vec::new(),
        nodes,
        up_axis: UpAxis::Y,
        load_time: Duration::ZERO,
    }
}

fn uv_sphere(radius: f32, segments: u32, rings: u32, material: usize) -> Primitive {
    let mut vertices = Vec::new();
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let theta = v * PI;
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let phi = u * 2.0 * PI;
            let n = Vec3::new(
                theta.sin() * phi.cos(),
                theta.cos(),
                theta.sin() * phi.sin(),
            );
            vertices.push(Vertex {
                position: (n * radius).into(),
                normal: n.into(),
                uv: [u, v],
            });
        }
    }
    let stride = segments + 1;
    let mut indices = Vec::new();
    for r in 0..rings {
        for s in 0..segments {
            let a = r * stride + s;
            let b = a + stride;
            indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    Primitive::new(vertices, indices, material)
}

fn cube(material: usize) -> Primitive {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for axis in 0..3 {
        for sign in [-1.0f32, 1.0] {
            let n = Vec3::from({
                let mut a = [0.0; 3];
                a[axis] = sign;
                a
            });
            let t = Vec3::from({
                let mut a = [0.0; 3];
                a[(axis + 1) % 3] = 1.0;
                a
            });
            let b = n.cross(t);
            let base = vertices.len() as u32;
            for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                let p = n + t * (u * 2.0 - 1.0) + b * (v * 2.0 - 1.0);
                vertices.push(Vertex {
                    position: (p * 0.5).into(),
                    normal: [0.0; 3],
                    uv: [u, v],
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    generate_normals(&mut vertices, &indices);
    Primitive::new(vertices, indices, material)
}
