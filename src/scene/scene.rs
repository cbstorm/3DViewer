use std::{path::PathBuf, time::Duration};

use glam::Mat4;

use super::mesh::{Aabb, Mesh};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlphaMode {
    Opaque,
    Mask(f32),
    Blend,
}

#[derive(Debug, Clone)]
pub struct Material {
    pub name: String,
    /// Linear RGBA.
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    /// Linear RGB, pre-multiplied by any emissive strength.
    pub emissive: [f32; 3],
    pub base_color_texture: Option<usize>,
    /// glTF convention: roughness in G, metallic in B.
    pub metallic_roughness_texture: Option<usize>,
    pub emissive_texture: Option<usize>,
    pub alpha_mode: AlphaMode,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            name: "Default".into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0; 3],
            base_color_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            alpha_mode: AlphaMode::Opaque,
        }
    }
}

/// Decoded RGBA8 texture.
#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Color data (base color, emissive) is sRGB; data textures are linear.
    pub srgb: bool,
}

/// An instance of a mesh in the world. Hierarchies are flattened at import
/// time; `depth` keeps the original nesting for display.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub mesh: usize,
    pub transform: Mat4,
    pub depth: usize,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpAxis {
    Y,
    Z,
}

impl UpAxis {
    /// Rotation that maps this up axis onto the renderer's +Y.
    pub fn root_transform(self) -> Mat4 {
        match self {
            UpAxis::Y => Mat4::IDENTITY,
            UpAxis::Z => Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scene {
    pub name: String,
    pub source: Option<PathBuf>,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub nodes: Vec<Node>,
    pub up_axis: UpAxis,
    pub load_time: Duration,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SceneStats {
    pub nodes: usize,
    pub meshes: usize,
    pub primitives: usize,
    pub vertices: usize,
    pub triangles: usize,
}

impl Scene {
    pub fn root_transform(&self) -> Mat4 {
        self.up_axis.root_transform()
    }

    pub fn world_transform(&self, node: &Node) -> Mat4 {
        self.root_transform() * node.transform
    }

    pub fn node_bounds(&self, node: &Node) -> Aabb {
        self.meshes[node.mesh]
            .bounds()
            .transformed(&self.world_transform(node))
    }

    /// World-space bounds of all visible nodes.
    pub fn bounds(&self) -> Aabb {
        let b = self
            .nodes
            .iter()
            .filter(|n| n.visible)
            .fold(Aabb::EMPTY, |b, n| b.union(self.node_bounds(n)));
        if b.is_empty() {
            Aabb {
                min: glam::Vec3::splat(-1.0),
                max: glam::Vec3::splat(1.0),
            }
        } else {
            b
        }
    }

    /// Instanced counts: a mesh referenced by two nodes counts twice.
    pub fn stats(&self) -> SceneStats {
        let mut s = SceneStats {
            nodes: self.nodes.len(),
            meshes: self.meshes.len(),
            primitives: self.meshes.iter().map(|m| m.primitives.len()).sum(),
            ..Default::default()
        };
        for node in &self.nodes {
            for p in &self.meshes[node.mesh].primitives {
                s.vertices += p.vertex_count;
                s.triangles += p.triangle_count();
            }
        }
        s
    }

    /// Make sure every primitive points at a valid material.
    pub fn ensure_default_material(&mut self) {
        let needs_default = self
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .any(|p| p.material >= self.materials.len());
        if needs_default {
            let fallback = self.materials.len();
            self.materials.push(Material::default());
            for p in self.meshes.iter_mut().flat_map(|m| &mut m.primitives) {
                if p.material > fallback {
                    p.material = fallback;
                }
            }
        }
    }
}
