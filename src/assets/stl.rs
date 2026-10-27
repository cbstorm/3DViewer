use std::{fs::File, io::BufReader, path::Path, time::Duration};

use anyhow::Result;
use glam::{Mat4, Vec3};

use super::import::deduplicate;
use crate::scene::{Material, Mesh, Node, Primitive, Scene, UpAxis, Vertex};

pub fn load(path: &Path) -> Result<Scene> {
    let mut reader = BufReader::new(File::open(path)?);
    let stl = stl_io::read_stl(&mut reader)?;

    // STL describes faceted geometry, so shade flat: each face gets its own
    // normal (recomputed from the winding, since stored normals are often
    // zero or stale), and identical corners are merged afterwards.
    let corners = stl.faces.iter().flat_map(|face| {
        let p = face.vertices.map(|i| Vec3::from(stl.vertices[i].0));
        let normal = (p[1] - p[0])
            .cross(p[2] - p[0])
            .try_normalize()
            .or_else(|| Vec3::from(face.normal.0).try_normalize())
            .unwrap_or(Vec3::Z);
        p.map(|position| Vertex {
            position: position.into(),
            normal: normal.into(),
            uv: [0.0; 2],
        })
    });
    let (vertices, indices) = deduplicate(corners);

    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "STL".into());
    Ok(Scene {
        name: name.clone(),
        source: None,
        meshes: vec![Mesh {
            name: name.clone(),
            primitives: vec![Primitive::new(vertices, indices, 0)],
        }],
        materials: vec![Material {
            name: "STL default".into(),
            base_color: [0.7, 0.72, 0.75, 1.0],
            metallic: 0.1,
            roughness: 0.5,
            ..Default::default()
        }],
        textures: Vec::new(),
        nodes: vec![Node {
            name,
            mesh: 0,
            transform: Mat4::IDENTITY,
            depth: 0,
            visible: true,
        }],
        // STL comes from CAD and 3D-printing tools, which are overwhelmingly Z-up.
        up_axis: UpAxis::Z,
        load_time: Duration::ZERO,
    })
}
