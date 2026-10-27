use anyhow::Result;
use ash::vk;
use gpu_allocator::MemoryLocation;

use super::{
    descriptors::{Descriptors, MATERIAL_TEXTURES},
    device::Gpu,
    memory::{Buffer, Image, ImageDesc, SharedAllocator},
    upload::{mip_levels_for, record_texture_upload, Uploader},
};
use crate::scene::{Scene, Texture, Vertex};

/// Location of one primitive inside the shared vertex and index buffers.
#[derive(Debug, Clone, Copy)]
pub struct GpuPrimitive {
    pub first_index: u32,
    pub index_count: u32,
    pub vertex_offset: i32,
}

/// Device-local copy of a scene. All geometry lives in one vertex buffer and
/// one index buffer, so drawing never rebinds buffers.
pub struct GpuScene {
    vertex_buffer: Buffer,
    index_buffer: Buffer,
    textures: Vec<Image>,
    material_pool: vk::DescriptorPool,
    pub material_sets: Vec<vk::DescriptorSet>,
    /// Indexed like `Scene::meshes[i].primitives[j]`.
    pub meshes: Vec<Vec<GpuPrimitive>>,
    pub memory_bytes: u64,
}

/// 1x1 white textures bound where a material has no texture.
pub struct DefaultTextures {
    pub srgb: Image,
    pub linear: Image,
}

impl DefaultTextures {
    pub fn new(gpu: &Gpu, allocator: &SharedAllocator, uploader: &Uploader) -> Result<Self> {
        let white = |srgb| Texture {
            name: "white".into(),
            width: 1,
            height: 1,
            rgba: vec![255; 4],
            srgb,
        };
        let mut images = upload_textures(gpu, allocator, uploader, &[white(true), white(false)])?;
        let linear = images.pop().unwrap();
        let srgb = images.pop().unwrap();
        Ok(DefaultTextures { srgb, linear })
    }

    pub fn destroy(&mut self, device: &ash::Device, allocator: &SharedAllocator) {
        self.srgb.destroy(device, allocator);
        self.linear.destroy(device, allocator);
    }
}

fn align(offset: usize, alignment: usize) -> usize {
    offset.div_ceil(alignment) * alignment
}

fn create_texture_image(gpu: &Gpu, allocator: &SharedAllocator, t: &Texture) -> Result<Image> {
    let format = if t.srgb {
        vk::Format::R8G8B8A8_SRGB
    } else {
        vk::Format::R8G8B8A8_UNORM
    };
    Image::new(
        &gpu.device,
        allocator,
        &ImageDesc {
            name: "texture",
            extent: vk::Extent2D {
                width: t.width,
                height: t.height,
            },
            format,
            usage: vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC,
            aspect: vk::ImageAspectFlags::COLOR,
            samples: vk::SampleCountFlags::TYPE_1,
            mip_levels: mip_levels_for(gpu, format, t.width, t.height),
        },
    )
}

fn upload_textures(
    gpu: &Gpu,
    allocator: &SharedAllocator,
    uploader: &Uploader,
    textures: &[Texture],
) -> Result<Vec<Image>> {
    let mut images = Vec::new();
    let total: usize = textures.iter().map(|t| align(t.rgba.len(), 16)).sum();
    let mut staging = Buffer::new(
        &gpu.device,
        allocator,
        "texture staging",
        total as u64,
        vk::BufferUsageFlags::TRANSFER_SRC,
        MemoryLocation::CpuToGpu,
    )?;
    let mut offsets = Vec::new();
    let mut offset = 0;
    for t in textures {
        staging.write(offset, &t.rgba);
        offsets.push(offset as u64);
        offset = align(offset + t.rgba.len(), 16);
        images.push(create_texture_image(gpu, allocator, t)?);
    }
    let result = uploader.submit(gpu, |cmd| {
        for (image, &offset) in images.iter().zip(&offsets) {
            record_texture_upload(&gpu.device, cmd, staging.buffer, offset, image);
        }
    });
    staging.destroy(&gpu.device, allocator);
    result?;
    Ok(images)
}

impl GpuScene {
    pub fn upload(
        gpu: &Gpu,
        allocator: &SharedAllocator,
        uploader: &Uploader,
        descriptors: &Descriptors,
        defaults: &DefaultTextures,
        scene: &Scene,
    ) -> Result<Self> {
        let device = &gpu.device;

        // Lay out every primitive back to back.
        let mut meshes = Vec::with_capacity(scene.meshes.len());
        let (mut vertex_count, mut index_count) = (0usize, 0usize);
        for mesh in &scene.meshes {
            let mut prims = Vec::with_capacity(mesh.primitives.len());
            for p in &mesh.primitives {
                prims.push(GpuPrimitive {
                    first_index: index_count as u32,
                    index_count: p.indices.len() as u32,
                    vertex_offset: vertex_count as i32,
                });
                vertex_count += p.vertices.len();
                index_count += p.indices.len();
            }
            meshes.push(prims);
        }
        let vertex_bytes = vertex_count * std::mem::size_of::<Vertex>();
        let index_bytes = index_count * std::mem::size_of::<u32>();

        let usage = vk::BufferUsageFlags::TRANSFER_DST;
        let mut vertex_buffer = Buffer::new(
            device,
            allocator,
            "scene vertices",
            vertex_bytes as u64,
            usage | vk::BufferUsageFlags::VERTEX_BUFFER,
            MemoryLocation::GpuOnly,
        )?;
        let mut index_buffer = match Buffer::new(
            device,
            allocator,
            "scene indices",
            index_bytes as u64,
            usage | vk::BufferUsageFlags::INDEX_BUFFER,
            MemoryLocation::GpuOnly,
        ) {
            Ok(b) => b,
            Err(e) => {
                vertex_buffer.destroy(device, allocator);
                return Err(e);
            }
        };

        let geometry = (|| -> Result<()> {
            let mut staging = Buffer::new(
                device,
                allocator,
                "geometry staging",
                (align(vertex_bytes, 16) + index_bytes) as u64,
                vk::BufferUsageFlags::TRANSFER_SRC,
                MemoryLocation::CpuToGpu,
            )?;
            let index_base = align(vertex_bytes, 16);
            let (mut v_off, mut i_off) = (0, index_base);
            for p in scene.meshes.iter().flat_map(|m| &m.primitives) {
                staging.write(v_off, &p.vertices);
                staging.write(i_off, &p.indices);
                v_off += std::mem::size_of_val(p.vertices.as_slice());
                i_off += std::mem::size_of_val(p.indices.as_slice());
            }
            let result = uploader.submit(gpu, |cmd| unsafe {
                if vertex_bytes > 0 {
                    let region = vk::BufferCopy {
                        src_offset: 0,
                        dst_offset: 0,
                        size: vertex_bytes as u64,
                    };
                    device.cmd_copy_buffer(cmd, staging.buffer, vertex_buffer.buffer, &[region]);
                }
                if index_bytes > 0 {
                    let region = vk::BufferCopy {
                        src_offset: index_base as u64,
                        dst_offset: 0,
                        size: index_bytes as u64,
                    };
                    device.cmd_copy_buffer(cmd, staging.buffer, index_buffer.buffer, &[region]);
                }
                // Make the copies visible to vertex input.
                let barrier = vk::MemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(
                        vk::AccessFlags::VERTEX_ATTRIBUTE_READ | vk::AccessFlags::INDEX_READ,
                    );
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::VERTEX_INPUT,
                    vk::DependencyFlags::empty(),
                    &[barrier],
                    &[],
                    &[],
                );
            });
            staging.destroy(device, allocator);
            result
        })();
        let textures = geometry.and_then(|_| {
            if scene.textures.is_empty() {
                Ok(Vec::new())
            } else {
                upload_textures(gpu, allocator, uploader, &scene.textures)
            }
        });
        let textures = match textures {
            Ok(t) => t,
            Err(e) => {
                vertex_buffer.destroy(device, allocator);
                index_buffer.destroy(device, allocator);
                return Err(e);
            }
        };

        let material_pool =
            Descriptors::create_material_pool(device, scene.materials.len() as u32)?;
        let view = |tex: Option<usize>, default: &Image| {
            tex.and_then(|i| textures.get(i))
                .map_or(default.view, |t| t.view)
        };
        let material_sets = scene
            .materials
            .iter()
            .map(|m| {
                let views: [vk::ImageView; MATERIAL_TEXTURES] = [
                    view(m.base_color_texture, &defaults.srgb),
                    view(m.metallic_roughness_texture, &defaults.linear),
                    view(m.emissive_texture, &defaults.srgb),
                ];
                descriptors.allocate_material_set(device, material_pool, views)
            })
            .collect::<Result<Vec<_>>>()?;

        let texture_bytes: u64 = scene
            .textures
            .iter()
            // A full mip chain adds about a third.
            .map(|t| t.rgba.len() as u64 * 4 / 3)
            .sum();
        Ok(GpuScene {
            vertex_buffer,
            index_buffer,
            textures,
            material_pool,
            material_sets,
            meshes,
            memory_bytes: (vertex_bytes + index_bytes) as u64 + texture_bytes,
        })
    }

    pub fn bind_geometry(&self, device: &ash::Device, cmd: vk::CommandBuffer) {
        unsafe {
            device.cmd_bind_vertex_buffers(cmd, 0, &[self.vertex_buffer.buffer], &[0]);
            device.cmd_bind_index_buffer(cmd, self.index_buffer.buffer, 0, vk::IndexType::UINT32);
        }
    }

    pub fn destroy(&mut self, device: &ash::Device, allocator: &SharedAllocator) {
        unsafe { device.destroy_descriptor_pool(self.material_pool, None) };
        for t in &mut self.textures {
            t.destroy(device, allocator);
        }
        self.vertex_buffer.destroy(device, allocator);
        self.index_buffer.destroy(device, allocator);
    }
}
