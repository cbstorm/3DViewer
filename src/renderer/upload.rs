use anyhow::Result;
use ash::vk;

use super::{device::Gpu, memory::Image};

/// Records one-off transfer work and blocks until the GPU finishes it.
/// Model loads are rare, so simplicity wins over a ring of staging buffers.
pub struct Uploader {
    pub pool: vk::CommandPool,
    fence: vk::Fence,
}

impl Uploader {
    pub fn new(gpu: &Gpu) -> Result<Self> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gpu.queue_family)
            .flags(
                vk::CommandPoolCreateFlags::TRANSIENT
                    | vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            );
        let pool = unsafe { gpu.device.create_command_pool(&pool_info, None)? };
        let fence = unsafe {
            gpu.device
                .create_fence(&vk::FenceCreateInfo::default(), None)?
        };
        Ok(Uploader { pool, fence })
    }

    pub fn submit(&self, gpu: &Gpu, record: impl FnOnce(vk::CommandBuffer)) -> Result<()> {
        let device = &gpu.device;
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { device.allocate_command_buffers(&alloc_info)?[0] };
        let result = (|| unsafe {
            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            record(cmd);
            device.end_command_buffer(cmd)?;
            let cmds = [cmd];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            device.queue_submit(gpu.queue, &[submit], self.fence)?;
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
            device.reset_fences(&[self.fence])?;
            Ok(())
        })();
        unsafe { device.free_command_buffers(self.pool, &[cmd]) };
        result
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}

/// Mip count for a full chain, or 1 if the format can't be blitted with linear filtering.
pub fn mip_levels_for(gpu: &Gpu, format: vk::Format, width: u32, height: u32) -> u32 {
    let props = unsafe {
        gpu.instance
            .get_physical_device_format_properties(gpu.physical_device, format)
    };
    let needed = vk::FormatFeatureFlags::BLIT_SRC
        | vk::FormatFeatureFlags::BLIT_DST
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
    if props.optimal_tiling_features.contains(needed) {
        32 - width.max(height).max(1).leading_zeros()
    } else {
        1
    }
}

fn mip_barrier(
    image: vk::Image,
    level: u32,
    count: u32,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .image(image)
        .old_layout(old)
        .new_layout(new)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: level,
            level_count: count,
            base_array_layer: 0,
            layer_count: 1,
        })
}

/// Copy staged pixels into mip 0, build the rest of the chain with blits,
/// and leave every level in SHADER_READ_ONLY_OPTIMAL.
pub fn record_texture_upload(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    staging: vk::Buffer,
    offset: vk::DeviceSize,
    image: &Image,
) {
    use vk::{AccessFlags as A, ImageLayout as L, PipelineStageFlags as S};
    let img = image.image;
    let levels = image.mip_levels;
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            S::TOP_OF_PIPE,
            S::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[mip_barrier(
                img,
                0,
                levels,
                L::UNDEFINED,
                L::TRANSFER_DST_OPTIMAL,
                A::empty(),
                A::TRANSFER_WRITE,
            )],
        );
        let region = vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: image.extent.width,
                height: image.extent.height,
                depth: 1,
            });
        device.cmd_copy_buffer_to_image(cmd, staging, img, L::TRANSFER_DST_OPTIMAL, &[region]);

        let (mut w, mut h) = (image.extent.width as i32, image.extent.height as i32);
        for level in 1..levels {
            device.cmd_pipeline_barrier(
                cmd,
                S::TRANSFER,
                S::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[mip_barrier(
                    img,
                    level - 1,
                    1,
                    L::TRANSFER_DST_OPTIMAL,
                    L::TRANSFER_SRC_OPTIMAL,
                    A::TRANSFER_WRITE,
                    A::TRANSFER_READ,
                )],
            );
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let layers = |mip_level| vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level,
                base_array_layer: 0,
                layer_count: 1,
            };
            let blit = vk::ImageBlit::default()
                .src_subresource(layers(level - 1))
                .src_offsets([vk::Offset3D::default(), vk::Offset3D { x: w, y: h, z: 1 }])
                .dst_subresource(layers(level))
                .dst_offsets([vk::Offset3D::default(), vk::Offset3D { x: nw, y: nh, z: 1 }]);
            device.cmd_blit_image(
                cmd,
                img,
                L::TRANSFER_SRC_OPTIMAL,
                img,
                L::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
            device.cmd_pipeline_barrier(
                cmd,
                S::TRANSFER,
                S::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[mip_barrier(
                    img,
                    level - 1,
                    1,
                    L::TRANSFER_SRC_OPTIMAL,
                    L::SHADER_READ_ONLY_OPTIMAL,
                    A::TRANSFER_READ,
                    A::SHADER_READ,
                )],
            );
            (w, h) = (nw, nh);
        }
        device.cmd_pipeline_barrier(
            cmd,
            S::TRANSFER,
            S::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[mip_barrier(
                img,
                levels - 1,
                1,
                L::TRANSFER_DST_OPTIMAL,
                L::SHADER_READ_ONLY_OPTIMAL,
                A::TRANSFER_WRITE,
                A::SHADER_READ,
            )],
        );
    }
}
