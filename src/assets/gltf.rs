use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use glam::{Mat4, Quat, Vec3};
use gltf::{animation::util::ReadOutputs, json::validation::Validate, mesh::Mode, Document, Gltf};

use crate::scene::{
    animation::{Animation, Channel, Interpolation, MorphTarget, Property, RigNode, Skin, Trs},
    generate_normals, AlphaMode, Deform, Material, Mesh, Node, Primitive, Rig, Scene, Texture,
    UpAxis, Vertex,
};

const KHR_DRACO: &str = "KHR_draco_mesh_compression";

pub fn load(path: &Path) -> Result<Scene> {
    let bytes = std::fs::read(path)?;
    // `gltf::import` rejects Draco files: the extension is unknown to the crate and
    // the compressed accessors have no bufferView. Validate ourselves instead and
    // excuse exactly those errors.
    let Gltf { document, blob } = Gltf::from_slice_without_validation(&bytes)?;
    validate(&document)?;
    let base = path.parent().unwrap_or_else(|| Path::new("./"));
    let buffers = gltf::import_buffers(&document, Some(base), blob)?;
    let images = gltf::import_images(&document, Some(base), &buffers)?;

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
            .filter_map(|p| match convert_primitive(&document, &p, &buffers) {
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
    let mut instancer = Instancer {
        mesh_map: &mesh_map,
        meshes: &mut meshes,
        deformed_in_use: HashSet::new(),
        out: &mut nodes,
    };
    match document
        .default_scene()
        .or_else(|| document.scenes().next())
    {
        Some(scene) => {
            for root in scene.nodes() {
                instancer.visit(&root, Mat4::IDENTITY, 0);
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
                    source: None,
                    skin: None,
                });
            }
        }
    }

    let deformed = meshes
        .iter()
        .flat_map(|m| &m.primitives)
        .any(|p| p.deform.is_some());
    let rig = (deformed || document.animations().len() > 0 || document.skins().len() > 0)
        .then(|| load_rig(&document, &buffers));

    Ok(Scene {
        name: String::new(),
        source: None,
        meshes,
        materials,
        textures: textures.textures,
        nodes,
        up_axis: UpAxis::Y,
        load_time: Duration::ZERO,
        rig,
    })
}

/// Flattens the scene graph into mesh instances.
struct Instancer<'a> {
    /// glTF mesh index -> our mesh index.
    mesh_map: &'a [Option<usize>],
    meshes: &'a mut Vec<Mesh>,
    /// Deformed meshes already claimed by an instance. Deformed vertices are
    /// per instance, so a second instance gets its own copy of the mesh.
    deformed_in_use: HashSet<usize>,
    out: &'a mut Vec<Node>,
}

impl Instancer<'_> {
    fn visit(&mut self, node: &gltf::Node, parent: Mat4, depth: usize) {
        let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mut mesh) = node.mesh().and_then(|m| self.mesh_map[m.index()]) {
            let prims = &self.meshes[mesh].primitives;
            if prims.iter().any(|p| p.deform.is_some()) && !self.deformed_in_use.insert(mesh) {
                let copy = self.meshes[mesh].clone();
                self.meshes.push(copy);
                mesh = self.meshes.len() - 1;
                self.deformed_in_use.insert(mesh);
            }
            let prims = &self.meshes[mesh].primitives;
            // Skinning needs influences on every primitive: the node transform
            // is dropped for skinned meshes, so an unskinned part would misplace.
            let skinned = prims
                .iter()
                .all(|p| p.deform.as_ref().is_some_and(|d| d.is_skinned()));
            let skin = node.skin().filter(|_| skinned).map(|s| s.index());
            if node.skin().is_some() && skin.is_none() {
                tracing::warn!(
                    "node {}: skin ignored, mesh has no joint weights",
                    node.index()
                );
            }
            self.out.push(Node {
                name: node
                    .name()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Node {}", node.index())),
                mesh,
                transform,
                depth,
                visible: true,
                source: Some(node.index()),
                skin,
            });
        }
        for child in node.children() {
            self.visit(&child, transform, depth + 1);
        }
    }
}

fn load_rig(document: &Document, buffers: &[gltf::buffer::Data]) -> Rig {
    let get = |b: gltf::Buffer| buffers.get(b.index()).map(|d| &d.0[..]);

    let mut nodes: Vec<RigNode> = document
        .nodes()
        .map(|n| {
            let (t, r, s) = n.transform().decomposed();
            RigNode {
                parent: None,
                rest_matrix: Mat4::from_cols_array_2d(&n.transform().matrix()),
                rest: Trs {
                    translation: Vec3::from(t),
                    rotation: Quat::from_array(r).normalize(),
                    scale: Vec3::from(s),
                },
                weights: n
                    .weights()
                    .or_else(|| n.mesh().and_then(|m| m.weights()))
                    .unwrap_or_default()
                    .to_vec(),
            }
        })
        .collect();
    for n in document.nodes() {
        for child in n.children() {
            // A node with two parents is invalid glTF; keep the first.
            nodes[child.index()].parent.get_or_insert(n.index());
        }
    }

    let skins = document
        .skins()
        .map(|skin| {
            let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
            let mut inverse_bind: Vec<Mat4> = skin
                .reader(get)
                .read_inverse_bind_matrices()
                .map(|m| m.map(|m| Mat4::from_cols_array_2d(&m)).collect())
                .unwrap_or_default();
            inverse_bind.resize(joints.len(), Mat4::IDENTITY);
            Skin {
                joints,
                inverse_bind,
            }
        })
        .collect();

    let animations = document
        .animations()
        .map(|anim| {
            let channels = anim
                .channels()
                .filter_map(|ch| {
                    let reader = ch.reader(get);
                    let times: Vec<f32> = reader.read_inputs()?.collect();
                    let (property, values): (Property, Vec<f32>) = match reader.read_outputs()? {
                        ReadOutputs::Translations(v) => {
                            (Property::Translation, v.flatten().collect())
                        }
                        ReadOutputs::Rotations(v) => {
                            (Property::Rotation, v.into_f32().flatten().collect())
                        }
                        ReadOutputs::Scales(v) => (Property::Scale, v.flatten().collect()),
                        ReadOutputs::MorphTargetWeights(v) => {
                            (Property::Weights, v.into_f32().collect())
                        }
                    };
                    let interpolation = match ch.sampler().interpolation() {
                        gltf::animation::Interpolation::Step => Interpolation::Step,
                        gltf::animation::Interpolation::Linear => Interpolation::Linear,
                        gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
                    };
                    let channel = Channel::new(
                        ch.target().node().index(),
                        property,
                        interpolation,
                        times,
                        values,
                    );
                    if channel.is_none() {
                        tracing::warn!(
                            "animation {}: skipping channel with mismatched keyframes",
                            anim.index()
                        );
                    }
                    channel
                })
                .collect();
            let name = anim
                .name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Animation {}", anim.index()));
            Animation::new(name, channels)
        })
        .collect();

    Rig::new(nodes, skins, animations)
}

/// Run the glTF crate's validation, ignoring only what Draco compression explains:
/// the required-extension check for KHR_draco_mesh_compression and the missing
/// bufferView on accessors whose data lives in a Draco blob.
fn validate(document: &Document) -> Result<()> {
    let root = document.as_json();
    let mut draco_accessors = HashSet::new();
    for mesh in &root.meshes {
        for p in &mesh.primitives {
            if draco_extension(p).is_some() {
                draco_accessors.extend(p.attributes.values().map(|a| a.value()));
                draco_accessors.extend(p.indices.map(|i| i.value()));
            }
        }
    }

    let mut errors = Vec::new();
    root.validate(root, gltf::json::Path::new, &mut |path, error| {
        let path = path().0;
        let excused = match error {
            gltf::json::validation::Error::Unsupported => {
                path.starts_with("extensionsRequired") && path.contains(KHR_DRACO)
            }
            gltf::json::validation::Error::Missing => path
                .strip_prefix("accessors[")
                .and_then(|rest| rest.strip_suffix("].bufferView"))
                .and_then(|i| i.parse::<usize>().ok())
                .is_some_and(|i| draco_accessors.contains(&i)),
            _ => false,
        };
        if !excused {
            errors.push(format!("{path}: {error}"));
        }
    });
    if errors.is_empty() {
        return Ok(());
    }
    const SHOWN: usize = 8;
    let mut msg = format!("invalid glTF ({} errors)", errors.len());
    for e in errors.iter().take(SHOWN) {
        msg.push_str("\n  ");
        msg.push_str(e);
    }
    if errors.len() > SHOWN {
        msg.push_str(&format!("\n  … and {} more", errors.len() - SHOWN));
    }
    Err(anyhow!(msg))
}

fn draco_extension(p: &gltf::json::mesh::Primitive) -> Option<&gltf::json::Value> {
    p.extensions.as_ref()?.others.get(KHR_DRACO)
}

/// Decoded (or plainly read) geometry of one primitive, before topology conversion.
struct Geometry {
    vertices: Vec<Vertex>,
    has_normals: bool,
    /// `None` means non-indexed.
    indices: Option<Vec<u32>>,
    /// JOINTS_0 / WEIGHTS_0.
    influences: Option<Vec<([u16; 4], [f32; 4])>>,
    targets: Vec<MorphTarget>,
}

fn read_plain(primitive: &gltf::Primitive, buffers: &[gltf::buffer::Data]) -> Option<Geometry> {
    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
    let mut vertices: Vec<Vertex> = reader
        .read_positions()?
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
    let indices = reader.read_indices().map(|i| i.into_u32().collect());
    let influences = match (reader.read_joints(0), reader.read_weights(0)) {
        (Some(j), Some(w)) => Some(j.into_u16().zip(w.into_f32()).collect()),
        _ => None,
    };
    let targets = reader
        .read_morph_targets()
        .map(|(positions, normals, _)| MorphTarget {
            positions: positions.map(Iterator::collect),
            normals: normals.map(Iterator::collect),
        })
        .collect();
    Some(Geometry {
        vertices,
        has_normals,
        indices,
        influences,
        targets,
    })
}

fn read_draco(
    document: &Document,
    primitive: &gltf::Primitive,
    ext: &gltf::json::Value,
    buffers: &[gltf::buffer::Data],
) -> Result<Option<Geometry>> {
    let view_index = ext
        .get("bufferView")
        .and_then(|v| v.as_u64())
        .context("KHR_draco_mesh_compression: missing bufferView")? as usize;
    let view = document
        .views()
        .nth(view_index)
        .with_context(|| format!("KHR_draco_mesh_compression: no bufferView {view_index}"))?;
    let data = buffers
        .get(view.buffer().index())
        .and_then(|b| b.0.get(view.offset()..view.offset() + view.length()))
        .context("KHR_draco_mesh_compression: bufferView out of range")?;

    let mut mesh = draco_core::Mesh::new();
    draco_core::MeshDecoder::new()
        .decode(&mut draco_core::DecoderBuffer::new(data), &mut mesh)
        .map_err(|e| anyhow!("Draco decode failed: {e}"))?;
    let num_points = mesh.num_points();

    // The extension maps glTF attribute semantics to Draco attribute unique ids;
    // the primitive's own accessors describe the decoded type (e.g. normalization).
    let attr_ids = ext.get("attributes");
    let read = |semantic: gltf::Semantic, components: usize| -> Option<Vec<f32>> {
        let id = attr_ids?.get(semantic.to_string())?.as_u64()? as u32;
        let attribute = mesh.attribute_by_unique_id(id)?;
        let mut values = attribute.read_f32s(num_points, components);
        let accessor = primitive.get(&semantic);
        if accessor.as_ref().is_some_and(|a| a.normalized()) {
            let scale = match attribute.data_type() {
                draco_core::DataType::Int8 => 1.0 / 127.0,
                draco_core::DataType::Uint8 => 1.0 / 255.0,
                draco_core::DataType::Int16 => 1.0 / 32767.0,
                draco_core::DataType::Uint16 => 1.0 / 65535.0,
                _ => 1.0,
            };
            for v in &mut values {
                *v = (*v * scale).max(-1.0);
            }
        }
        Some(values)
    };

    let Some(positions) = read(gltf::Semantic::Positions, 3) else {
        return Ok(None);
    };
    let mut vertices: Vec<Vertex> = positions
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&position| Vertex {
            position,
            ..Default::default()
        })
        .collect();
    let normals = read(gltf::Semantic::Normals, 3);
    if let Some(normals) = &normals {
        for (v, &n) in vertices.iter_mut().zip(normals.as_chunks::<3>().0) {
            v.normal = n;
        }
    }
    if let Some(uvs) = read(gltf::Semantic::TexCoords(0), 2) {
        for (v, &uv) in vertices.iter_mut().zip(uvs.as_chunks::<2>().0) {
            v.uv = uv;
        }
    }
    let influences = match (
        read(gltf::Semantic::Joints(0), 4),
        read(gltf::Semantic::Weights(0), 4),
    ) {
        (Some(j), Some(w)) => Some(
            j.as_chunks::<4>()
                .0
                .iter()
                .zip(w.as_chunks::<4>().0)
                .map(|(j, &w)| (j.map(|j| j as u16), w))
                .collect(),
        ),
        _ => None,
    };
    let indices = mesh.faces().as_flattened().iter().map(|p| p.0).collect();
    // Morph targets can't be used: Draco reorders the vertices they refer to.
    if primitive.morph_targets().len() > 0 {
        tracing::warn!("Draco primitive: morph targets are not supported, ignoring them");
    }
    Ok(Some(Geometry {
        vertices,
        has_normals: normals.is_some(),
        indices: Some(indices),
        influences,
        targets: Vec::new(),
    }))
}

fn convert_primitive(
    document: &Document,
    primitive: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
) -> Result<Option<Primitive>> {
    let draco = primitive.extension_value(KHR_DRACO);
    let geometry = match draco {
        Some(ext) => read_draco(document, primitive, ext, buffers)?,
        None => read_plain(primitive, buffers),
    };
    let Some(Geometry {
        mut vertices,
        has_normals,
        indices,
        influences,
        targets,
    }) = geometry
    else {
        return Ok(None);
    };

    let n = vertices.len() as u32;
    let raw = indices.unwrap_or_else(|| (0..n).collect());
    if let Some(&bad) = raw.iter().find(|&&i| i >= n) {
        bail!("index {bad} out of range for {n} vertices");
    }
    // Draco always decodes to a triangle list.
    let mode = if draco.is_some() {
        Mode::Triangles
    } else {
        primitive.mode()
    };
    let indices = match mode {
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
    let deform = Deform::new(&vertices, influences, targets).map(Box::new);
    let mut p = Primitive::new(vertices, indices, material);
    p.deform = deform;
    Ok(Some(p))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::ClipSelection;

    /// A triangle skinned to one joint whose translation is animated from
    /// (0, 0, 0) to (0, 2, 0) over one second. The mesh node has a transform
    /// that must be ignored once skinned.
    fn write_skinned_gltf(dir: &Path) -> std::path::PathBuf {
        let mut bin: Vec<u8> = Vec::new();
        let f32s =
            |bin: &mut Vec<u8>, v: &[f32]| v.iter().for_each(|x| bin.extend(x.to_le_bytes()));
        f32s(&mut bin, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]); // 0: positions, 36 B
        for _ in 0..3 {
            [0u16, 0, 0, 0]
                .iter()
                .for_each(|j| bin.extend(j.to_le_bytes())); // 36: joints, 24 B
        }
        for _ in 0..3 {
            f32s(&mut bin, &[1.0, 0.0, 0.0, 0.0]); // 60: weights, 48 B
        }
        f32s(&mut bin, &[0.0, 1.0]); // 108: times, 8 B
        f32s(&mut bin, &[0.0, 0.0, 0.0, 0.0, 2.0, 0.0]); // 116: translations, 24 B
        assert_eq!(bin.len(), 140);
        std::fs::write(dir.join("skin.bin"), &bin).unwrap();

        let json = r#"{
          "asset": {"version": "2.0"},
          "scene": 0,
          "scenes": [{"nodes": [0, 1]}],
          "nodes": [
            {"name": "body", "mesh": 0, "skin": 0, "translation": [5, 5, 5]},
            {"name": "bone"}
          ],
          "skins": [{"joints": [1]}],
          "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2}}]}],
          "animations": [{
            "name": "lift",
            "samplers": [{"input": 3, "output": 4, "interpolation": "LINEAR"}],
            "channels": [{"sampler": 0, "target": {"node": 1, "path": "translation"}}]
          }],
          "buffers": [{"uri": "skin.bin", "byteLength": 140}],
          "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 24},
            {"buffer": 0, "byteOffset": 60, "byteLength": 48},
            {"buffer": 0, "byteOffset": 108, "byteLength": 8},
            {"buffer": 0, "byteOffset": 116, "byteLength": 24}
          ],
          "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]},
            {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 3, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0], "max": [1]},
            {"bufferView": 4, "componentType": 5126, "count": 2, "type": "VEC3"}
          ]
        }"#;
        let path = dir.join("skin.gltf");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn loads_and_poses_skinned_animation() {
        let dir = std::env::temp_dir().join(format!("viewer3d-test-skin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut scene = load(&write_skinned_gltf(&dir)).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let rig = scene.rig.as_ref().unwrap();
        assert_eq!(rig.animations.len(), 1);
        assert_eq!(rig.animations[0].name, "lift");
        assert_eq!(rig.duration(ClipSelection::One(0)), 1.0);
        assert_eq!(scene.nodes.len(), 1);
        assert_eq!(scene.nodes[0].skin, Some(0));

        scene.pose(ClipSelection::One(0), 0.5);
        // Skinned: the node's own translation is ignored, the joint moves the vertices.
        assert_eq!(scene.nodes[0].transform, Mat4::IDENTITY);
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.positions[1], Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(
            p.deform.as_ref().unwrap().output[2].position,
            [0.0, 2.0, 0.0]
        );
        assert_eq!(p.bounds.min.y, 1.0);

        scene.pose(ClipSelection::None, 0.5);
        assert_eq!(
            scene.meshes[0].primitives[0].positions[1],
            Vec3::new(1.0, 0.0, 0.0)
        );
    }
}
