use std::{collections::HashMap, path::Path, time::Duration};

use anyhow::Result;
use glam::Mat4;
use gltf::mesh::Mode;

use crate::scene::{
    generate_normals, AlphaMode, Material, Mesh, Node, Primitive, Scene, Texture, UpAxis, Vertex,
};

pub fn load(path: &Path) -> Result<Scene> {
    let (document, buffers, images) = gltf::import(path)?;

    let mut textures = Textures {
        images: &images,
        textures: Vec::new(),
        by_image: HashMap::new(),
    };
    let materials = document
        .materials()
        .map(|m| convert_material(&m, &mut textures))
        .collect();

    // glTF mesh index -> our mesh index; meshes without triangles are dropped.
    let mut mesh_map = vec![None; document.meshes().len()];
    let mut meshes = Vec::new();
    for mesh in document.meshes() {
        let primitives: Vec<Primitive> = mesh
            .primitives()
            .filter_map(|p| match convert_primitive(&p, &buffers) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("mesh {:?}: skipping primitive: {e}", mesh.name());
                    None
                }
            })
            .collect();
        if primitives.is_empty() {
            continue;
        }
        mesh_map[mesh.index()] = Some(meshes.len());
        meshes.push(Mesh {
            name: mesh
                .name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Mesh {}", mesh.index())),
            primitives,
        });
    }

    let mut nodes = Vec::new();
    match document
        .default_scene()
        .or_else(|| document.scenes().next())
    {
        Some(scene) => {
            for root in scene.nodes() {
                visit_node(&root, Mat4::IDENTITY, 0, &mesh_map, &mut nodes);
            }
        }
        // No scene graph: show each mesh once at the origin.
        None => {
            for (i, mesh) in meshes.iter().enumerate() {
                nodes.push(Node {
                    name: mesh.name.clone(),
                    mesh: i,
                    transform: Mat4::IDENTITY,
                    depth: 0,
                    visible: true,
                });
            }
        }
    }

    Ok(Scene {
        name: String::new(),
        source: None,
        meshes,
        materials,
        textures: textures.textures,
        nodes,
        up_axis: UpAxis::Y,
        load_time: Duration::ZERO,
    })
}

fn visit_node(
    node: &gltf::Node,
    parent: Mat4,
    depth: usize,
    mesh_map: &[Option<usize>],
    out: &mut Vec<Node>,
) {
    let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
    if let Some(mesh) = node.mesh().and_then(|m| mesh_map[m.index()]) {
        out.push(Node {
            name: node
                .name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Node {}", node.index())),
            mesh,
            transform,
            depth,
            visible: true,
        });
    }
    for child in node.children() {
        visit_node(&child, transform, depth + 1, mesh_map, out);
    }
}

fn convert_primitive(
    primitive: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
) -> Result<Option<Primitive>> {
    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
    let Some(positions) = reader.read_positions() else {
        return Ok(None);
    };
    let mut vertices: Vec<Vertex> = positions
        .map(|position| Vertex {
            position,
            ..Default::default()
        })
        .collect();

    let has_normals = match reader.read_normals() {
        Some(normals) => {
            for (v, n) in vertices.iter_mut().zip(normals) {
                v.normal = n;
            }
            true
        }
        None => false,
    };
    if let Some(uvs) = reader.read_tex_coords(0) {
        for (v, uv) in vertices.iter_mut().zip(uvs.into_f32()) {
            v.uv = uv;
        }
    }

    let n = vertices.len() as u32;
    let raw: Vec<u32> = match reader.read_indices() {
        Some(indices) => indices.into_u32().collect(),
        None => (0..n).collect(),
    };
    if let Some(&bad) = raw.iter().find(|&&i| i >= n) {
        anyhow::bail!("index {bad} out of range for {n} vertices");
    }
    let indices = match primitive.mode() {
        Mode::Triangles => raw,
        Mode::TriangleStrip => (0..raw.len().saturating_sub(2))
            .flat_map(|i| {
                if i % 2 == 0 {
                    [raw[i], raw[i + 1], raw[i + 2]]
                } else {
                    [raw[i + 1], raw[i], raw[i + 2]]
                }
            })
            .collect(),
        Mode::TriangleFan => (1..raw.len().saturating_sub(1))
            .flat_map(|i| [raw[0], raw[i], raw[i + 1]])
            .collect(),
        // Points and lines are not rendered by this viewer.
        _ => return Ok(None),
    };
    if !has_normals {
        generate_normals(&mut vertices, &indices);
    }

    let material = primitive.material().index().unwrap_or(usize::MAX);
    Ok(Some(Primitive::new(vertices, indices, material)))
}

struct Textures<'a> {
    images: &'a [gltf::image::Data],
    textures: Vec<Texture>,
    by_image: HashMap<(usize, bool), usize>,
}

impl Textures<'_> {
    fn get(&mut self, info: Option<gltf::texture::Texture>, srgb: bool) -> Option<usize> {
        let source = info?.source();
        let key = (source.index(), srgb);
        if let Some(&index) = self.by_image.get(&key) {
            return Some(index);
        }
        let data = self.images.get(source.index())?;
        let Some(rgba) = to_rgba8(data) else {
            tracing::warn!(
                "image {}: unsupported pixel format {:?}",
                source.index(),
                data.format
            );
            return None;
        };
        self.textures.push(Texture {
            name: source
                .name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Image {}", source.index())),
            width: data.width,
            height: data.height,
            rgba,
            srgb,
        });
        let index = self.textures.len() - 1;
        self.by_image.insert(key, index);
        Some(index)
    }
}

fn to_rgba8(data: &gltf::image::Data) -> Option<Vec<u8>> {
    use gltf::image::Format;
    let p = &data.pixels;
    // Keep the high byte of 16-bit channels; clamp float channels to [0, 1].
    let u16_to_u8 = |c: &[u8; 2]| (u16::from_le_bytes(*c) >> 8) as u8;
    let f32_to_u8 = |c: &[u8; 4]| (f32::from_le_bytes(*c).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let out = match data.format {
        Format::R8G8B8A8 => p.clone(),
        Format::R8G8B8 => p
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        Format::R8G8 => p
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[r, g]| [r, g, 0, 255])
            .collect(),
        Format::R8 => p.iter().flat_map(|&c| [c, c, c, 255]).collect(),
        Format::R16G16B16A16 | Format::R16G16B16 | Format::R16G16 | Format::R16 => {
            let channels = match data.format {
                Format::R16G16B16A16 => 4,
                Format::R16G16B16 => 3,
                Format::R16G16 => 2,
                _ => 1,
            };
            expand(p.as_chunks::<2>().0.iter().map(u16_to_u8), channels)
        }
        Format::R32G32B32A32FLOAT | Format::R32G32B32FLOAT => {
            let channels = if data.format == Format::R32G32B32A32FLOAT {
                4
            } else {
                3
            };
            expand(p.as_chunks::<4>().0.iter().map(f32_to_u8), channels)
        }
    };
    (out.len() == (data.width * data.height * 4) as usize).then_some(out)
}

/// Pad `channels`-wide pixels to RGBA; single-channel data becomes gray.
fn expand(values: impl Iterator<Item = u8>, channels: usize) -> Vec<u8> {
    let values: Vec<u8> = values.collect();
    values
        .chunks(channels)
        .flat_map(|c| match *c {
            [r, g, b, a] => [r, g, b, a],
            [r, g, b] => [r, g, b, 255],
            [r, g] => [r, g, 0, 255],
            [v] => [v, v, v, 255],
            _ => [0, 0, 0, 255],
        })
        .collect()
}

fn convert_material(m: &gltf::Material, textures: &mut Textures) -> Material {
    let pbr = m.pbr_metallic_roughness();
    let emissive_strength = m.emissive_strength().unwrap_or(1.0);
    Material {
        name: m
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Material {}", m.index().unwrap_or(0))),
        base_color: pbr.base_color_factor(),
        metallic: pbr.metallic_factor(),
        roughness: pbr.roughness_factor(),
        emissive: m.emissive_factor().map(|c| c * emissive_strength),
        base_color_texture: textures.get(pbr.base_color_texture().map(|i| i.texture()), true),
        metallic_roughness_texture: textures
            .get(pbr.metallic_roughness_texture().map(|i| i.texture()), false),
        emissive_texture: textures.get(m.emissive_texture().map(|i| i.texture()), true),
        alpha_mode: match m.alpha_mode() {
            gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
            gltf::material::AlphaMode::Mask => AlphaMode::Mask(m.alpha_cutoff().unwrap_or(0.5)),
            gltf::material::AlphaMode::Blend => AlphaMode::Blend,
        },
    }
}
