use anyhow::Result;
use ash::vk;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec4};

use super::device::Gpu;

/// Set 0: per-frame uniforms. Must match `Frame` in the shaders.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FrameUniforms {
    pub view_proj: Mat4,
    pub camera_pos: Vec4,
    pub light_dir: Vec4,
    pub light_color: Vec4,
    pub sky_color: Vec4,
    pub ground_color: Vec4,
}

/// Set 1 bindings: base color, metallic-roughness, emissive textures, then a sampler.
pub const MATERIAL_TEXTURES: usize = 3;

pub struct Descriptors {
    pub frame_layout: vk::DescriptorSetLayout,
    pub material_layout: vk::DescriptorSetLayout,
    frame_pool: vk::DescriptorPool,
    pub sampler: vk::Sampler,
}

impl Descriptors {
    pub fn new(gpu: &Gpu, frames: u32) -> Result<Self> {
        let device = &gpu.device;
        let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
        let frame_bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(stages)];
        let frame_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&frame_bindings),
                None,
            )?
        };

        let mut material_bindings: Vec<_> = (0..MATERIAL_TEXTURES as u32)
            .map(|b| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            })
            .collect();
        material_bindings.push(
            vk::DescriptorSetLayoutBinding::default()
                .binding(MATERIAL_TEXTURES as u32)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        );
        let material_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&material_bindings),
                None,
            )?
        };

        let pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: frames,
        }];
        let frame_pool = unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(frames)
                    .pool_sizes(&pool_sizes),
                None,
            )?
        };

        let max_anisotropy = gpu.properties.limits.max_sampler_anisotropy.min(16.0);
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::REPEAT)
            .address_mode_v(vk::SamplerAddressMode::REPEAT)
            .address_mode_w(vk::SamplerAddressMode::REPEAT)
            .anisotropy_enable(gpu.supports_anisotropy)
            .max_anisotropy(if gpu.supports_anisotropy {
                max_anisotropy
            } else {
                1.0
            })
            .max_lod(vk::LOD_CLAMP_NONE);
        let sampler = unsafe { device.create_sampler(&sampler_info, None)? };

        Ok(Descriptors {
            frame_layout,
            material_layout,
            frame_pool,
            sampler,
        })
    }

    pub fn allocate_frame_set(
        &self,
        device: &ash::Device,
        buffer: vk::Buffer,
    ) -> Result<vk::DescriptorSet> {
        let layouts = [self.frame_layout];
        let set = unsafe {
            device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.frame_pool)
                    .set_layouts(&layouts),
            )?[0]
        };
        let buffer_info = [vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range: std::mem::size_of::<FrameUniforms>() as u64,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&buffer_info);
        unsafe { device.update_descriptor_sets(&[write], &[]) };
        Ok(set)
    }

    /// A pool sized for one scene's materials; destroyed with the scene.
    pub fn create_material_pool(
        device: &ash::Device,
        materials: u32,
    ) -> Result<vk::DescriptorPool> {
        let materials = materials.max(1);
        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: materials * MATERIAL_TEXTURES as u32,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: materials,
            },
        ];
        Ok(unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(materials)
                    .pool_sizes(&pool_sizes),
                None,
            )?
        })
    }

    pub fn allocate_material_set(
        &self,
        device: &ash::Device,
        pool: vk::DescriptorPool,
        views: [vk::ImageView; MATERIAL_TEXTURES],
    ) -> Result<vk::DescriptorSet> {
        let layouts = [self.material_layout];
        let set = unsafe {
            device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&layouts),
            )?[0]
        };
        let image_infos: Vec<[vk::DescriptorImageInfo; 1]> = views
            .iter()
            .map(|&view| {
                [vk::DescriptorImageInfo {
                    sampler: vk::Sampler::null(),
                    image_view: view,
                    image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                }]
            })
            .collect();
        let sampler_info = [vk::DescriptorImageInfo {
            sampler: self.sampler,
            image_view: vk::ImageView::null(),
            image_layout: vk::ImageLayout::UNDEFINED,
        }];
        let mut writes: Vec<_> = image_infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info)
            })
            .collect();
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(MATERIAL_TEXTURES as u32)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&sampler_info),
        );
        unsafe { device.update_descriptor_sets(&writes, &[]) };
        Ok(set)
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            device.destroy_sampler(self.sampler, None);
            device.destroy_descriptor_pool(self.frame_pool, None);
            device.destroy_descriptor_set_layout(self.frame_layout, None);
            device.destroy_descriptor_set_layout(self.material_layout, None);
        }
    }
}
