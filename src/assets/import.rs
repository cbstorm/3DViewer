use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{bail, Context, Result};

use crate::scene::{Scene, Texture, Vertex};

pub const SUPPORTED_EXTENSIONS: &[&str] = &["gltf", "glb", "obj", "stl"];

pub fn is_supported(path: &Path) -> bool {
    extension(path).is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.as_str()))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Parse a model file into the format-independent scene representation.
/// Runs on a worker thread; does no GPU work.
pub fn load(path: &Path) -> Result<Scene> {
    let start = Instant::now();
    let mut scene = match extension(path).as_deref() {
        Some("gltf" | "glb") => super::gltf::load(path),
        Some("obj") => super::obj::load(path),
        Some("stl") => super::stl::load(path),
        _ => bail!(
            "unsupported file type (expected one of: {})",
            SUPPORTED_EXTENSIONS.join(", ")
        ),
    }
    .with_context(|| format!("failed to load {}", path.display()))?;

    if scene.nodes.is_empty() {
        bail!("{} contains no triangle meshes", path.display());
    }
    scene.ensure_default_material();
    scene.source = Some(path.to_path_buf());
    scene.name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    scene.load_time = start.elapsed();

    let stats = scene.stats();
    tracing::info!(
        "loaded {} in {:.0?}: {} nodes, {} primitives, {} vertices, {} triangles",
        path.display(),
        scene.load_time,
        stats.nodes,
        stats.primitives,
        stats.vertices,
        stats.triangles
    );
    Ok(scene)
}

/// Merge vertices with bit-identical attributes and return the index buffer.
pub fn deduplicate(vertices: impl IntoIterator<Item = Vertex>) -> (Vec<Vertex>, Vec<u32>) {
    let mut unique = Vec::new();
    let mut indices = Vec::new();
    let mut lookup: HashMap<[u32; 8], u32> = HashMap::new();
    for v in vertices {
        let key: [u32; 8] = bytemuck::cast(v);
        let index = *lookup.entry(key).or_insert_with(|| {
            unique.push(v);
            (unique.len() - 1) as u32
        });
        indices.push(index);
    }
    (unique, indices)
}

/// Decodes texture files referenced by path (OBJ/MTL), caching by path and color space.
pub struct TextureCache {
    base_dir: PathBuf,
    textures: Vec<Texture>,
    by_path: HashMap<(PathBuf, bool), usize>,
}

impl TextureCache {
    pub fn new(base_dir: &Path) -> Self {
        TextureCache {
            base_dir: base_dir.to_path_buf(),
            textures: Vec::new(),
            by_path: HashMap::new(),
        }
    }

    /// Missing or undecodable textures are logged and skipped rather than failing the load.
    pub fn load(&mut self, relative: &str, srgb: bool) -> Option<usize> {
        // MTL files may contain options before the file name, e.g. "-bm 1 normal.png".
        let name = relative.split_whitespace().last()?.replace('\\', "/");
        let path = self.base_dir.join(&name);
        if let Some(&index) = self.by_path.get(&(path.clone(), srgb)) {
            return Some(index);
        }
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                self.textures.push(Texture {
                    name,
                    width: rgba.width(),
                    height: rgba.height(),
                    rgba: rgba.into_raw(),
                    srgb,
                });
                let index = self.textures.len() - 1;
                self.by_path.insert((path, srgb), index);
                Some(index)
            }
            Err(e) => {
                tracing::warn!("texture {}: {e}", path.display());
                None
            }
        }
    }

    pub fn into_textures(self) -> Vec<Texture> {
        self.textures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{AlphaMode, UpAxis};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viewer3d-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn deduplicate_merges_identical_vertices() {
        let a = Vertex {
            position: [1.0, 2.0, 3.0],
            ..Default::default()
        };
        let b = Vertex {
            position: [4.0, 5.0, 6.0],
            ..Default::default()
        };
        let (vertices, indices) = deduplicate([a, b, a, a]);
        assert_eq!(vertices, vec![a, b]);
        assert_eq!(indices, vec![0, 1, 0, 0]);
    }

    #[test]
    fn rejects_unknown_extension() {
        assert!(!is_supported(Path::new("model.fbx")));
        assert!(is_supported(Path::new("MODEL.GLB")));
        assert!(load(Path::new("model.fbx")).is_err());
    }

    #[test]
    fn loads_ascii_stl_flat_shaded_and_z_up() {
        let dir = temp_dir("stl");
        let path = dir.join("tri.stl");
        std::fs::write(
            &path,
            "solid t\nfacet normal 0 0 0\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n",
        )
        .unwrap();
        let scene = load(&path).unwrap();
        assert_eq!(scene.up_axis, UpAxis::Z);
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.indices, vec![0, 1, 2]);
        // The zero normal in the file is replaced by one computed from the winding.
        assert!(p.vertices.iter().all(|v| v.normal == [0.0, 0.0, 1.0]));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn loads_obj_with_materials_and_generates_normals() {
        let dir = temp_dir("obj");
        std::fs::write(dir.join("m.mtl"), "newmtl Red\nKd 1 0 0\nd 0.5\n").unwrap();
        std::fs::write(
            dir.join("quad.obj"),
            "mtllib m.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nusemtl Red\nf 1/1 2/2 3/3 4/4\n",
        )
        .unwrap();
        let scene = load(&dir.join("quad.obj")).unwrap();
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.triangle_count(), 2);
        assert!(p.vertices.iter().all(|v| v.normal == [0.0, 0.0, 1.0]));
        // OBJ V runs bottom-up; it is flipped for top-down image rows.
        assert_eq!(p.vertices[0].uv, [0.0, 1.0]);
        let m = &scene.materials[p.material];
        assert_eq!(m.name, "Red");
        assert_eq!(m.alpha_mode, AlphaMode::Blend);
        assert_eq!(m.base_color, [1.0, 0.0, 0.0, 0.5]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn obj_without_materials_gets_default() {
        let dir = temp_dir("obj-nomtl");
        std::fs::write(dir.join("t.obj"), "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        let scene = load(&dir.join("t.obj")).unwrap();
        let p = &scene.meshes[0].primitives[0];
        assert!(p.material < scene.materials.len());
        std::fs::remove_dir_all(dir).ok();
    }
}
