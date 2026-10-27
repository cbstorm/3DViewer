use std::{path::Path, time::Duration};

use anyhow::Result;
use glam::Mat4;

use super::import::TextureCache;
use crate::scene::{
    generate_normals, AlphaMode, Material, Mesh, Node, Primitive, Scene, UpAxis, Vertex,
};

pub fn load(path: &Path) -> Result<Scene> {
    let options = tobj::LoadOptions {
        single_index: true,
        triangulate: true,
        ignore_points: true,
        ignore_lines: true,
    };
    let (models, materials) = tobj::load_obj(path, &options)?;
    let obj_materials = materials.unwrap_or_else(|e| {
        tracing::warn!("{}: material library: {e}", path.display());
        Vec::new()
    });

    let base_dir = path.parent().unwrap_or(Path::new("."));
    let mut textures = TextureCache::new(base_dir);
    let materials = obj_materials
        .iter()
        .map(|m| convert_material(m, &mut textures))
        .collect();

    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for model in models {
        let m = &model.mesh;
        if m.indices.is_empty() {
            continue;
        }
        let vertex_count = m.positions.len() / 3;
        let has_normals = m.normals.len() == m.positions.len();
        let has_uvs = m.texcoords.len() / 2 == vertex_count;

        let mut vertices: Vec<Vertex> = (0..vertex_count)
            .map(|i| Vertex {
                position: [
                    m.positions[3 * i],
                    m.positions[3 * i + 1],
                    m.positions[3 * i + 2],
                ],
                normal: if has_normals {
                    [m.normals[3 * i], m.normals[3 * i + 1], m.normals[3 * i + 2]]
                } else {
                    [0.0; 3]
                },
                // OBJ puts the UV origin at the bottom-left; images start at the top.
                uv: if has_uvs {
                    [m.texcoords[2 * i], 1.0 - m.texcoords[2 * i + 1]]
                } else {
                    [0.0; 2]
                },
            })
            .collect();
        if !has_normals {
            generate_normals(&mut vertices, &m.indices);
        }

        let material = m.material_id.unwrap_or(usize::MAX);
        let name = if model.name.is_empty() {
            format!("Object {}", meshes.len())
        } else {
            model.name.clone()
        };
        nodes.push(Node {
            name: name.clone(),
            mesh: meshes.len(),
            transform: Mat4::IDENTITY,
            depth: 0,
            visible: true,
        });
        meshes.push(Mesh {
            name,
            primitives: vec![Primitive::new(vertices, m.indices.clone(), material)],
        });
    }

    Ok(Scene {
        name: String::new(),
        source: None,
        meshes,
        materials,
        textures: textures.into_textures(),
        nodes,
        up_axis: UpAxis::Y,
        load_time: Duration::ZERO,
    })
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn convert_material(m: &tobj::Material, textures: &mut TextureCache) -> Material {
    let param = |key: &str| m.unknown_param.get(key);
    let param_f32 = |key: &str| param(key).and_then(|v| v.trim().parse::<f32>().ok());

    let diffuse = m.diffuse.unwrap_or([0.8; 3]).map(srgb_to_linear);
    let alpha = m.dissolve.unwrap_or(1.0).clamp(0.0, 1.0);
    // Map Blinn-Phong shininess onto roughness unless a PBR extension value is given.
    let roughness = param_f32("Pr").unwrap_or_else(|| {
        let ns = m.shininess.unwrap_or(32.0).max(0.0);
        (2.0 / (ns + 2.0)).sqrt()
    });
    let emissive = m.emissive.unwrap_or([0.0; 3]).map(srgb_to_linear);
    let emissive_texture = param("map_Ke").and_then(|t| textures.load(t, true));

    Material {
        name: m.name.clone(),
        base_color: [diffuse[0], diffuse[1], diffuse[2], alpha],
        metallic: param_f32("Pm").unwrap_or(0.0),
        roughness: roughness.clamp(0.04, 1.0),
        emissive: if emissive_texture.is_some() && emissive == [0.0; 3] {
            [1.0; 3]
        } else {
            emissive
        },
        base_color_texture: m
            .diffuse_texture
            .as_deref()
            .and_then(|t| textures.load(t, true)),
        metallic_roughness_texture: None,
        emissive_texture,
        alpha_mode: if alpha < 1.0 {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
    }
}
