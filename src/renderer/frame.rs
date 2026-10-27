use anyhow::Result;
use ash::vk;
use gpu_allocator::MemoryLocation;

use super::{
    descriptors::{Descriptors, FrameUniforms},
    device::Gpu,
    memory::{Buffer, SharedAllocator},
};

pub const FRAMES_IN_FLIGHT: usize = 2;

/// Resources owned by one frame in flight.
pub struct FrameResources {
    pool: vk::CommandPool,
    pub cmd: vk::CommandBuffer,
    pub image_available: vk::Semaphore,
    pub in_flight: vk::Fence,
    pub uniforms: Buffer,
    pub frame_set: vk::DescriptorSet,
    /// Two timestamps bracketing the frame's GPU work.
    pub queries: Option<vk::QueryPool>,
    pub queries_pending: bool,
}

impl FrameResources {
    pub fn new(gpu: &Gpu, allocator: &SharedAllocator, descriptors: &Descriptors) -> Result<Self> {
        let device = &gpu.device;
        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(gpu.queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?
        };
        let cmd = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0]
        };
        let image_available =
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };
        // Start signalled so the first wait returns immediately.
        let in_flight = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?
        };
        let uniforms = Buffer::new(
            device,
            allocator,
            "frame uniforms",
            std::mem::size_of::<FrameUniforms>() as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            MemoryLocation::CpuToGpu,
        )?;
        let frame_set = descriptors.allocate_frame_set(device, uniforms.buffer)?;
        let queries = if gpu.timestamps {
            Some(unsafe {
                device.create_query_pool(
                    &vk::QueryPoolCreateInfo::default()
                        .query_type(vk::QueryType::TIMESTAMP)
                        .query_count(2),
                    None,
                )?
            })
        } else {
            None
        };
        Ok(FrameResources {
            pool,
            cmd,
            image_available,
            in_flight,
            uniforms,
            frame_set,
            queries,
            queries_pending: false,
        })
    }

    /// GPU time of this frame's last submission, in milliseconds. Call after its fence signalled.
    pub fn read_gpu_time(&mut self, gpu: &Gpu) -> Option<f32> {
        let pool = self.queries?;
        if !self.queries_pending {
            return None;
        }
        self.queries_pending = false;
        let mut stamps = [0u64; 2];
        unsafe {
            gpu.device
                .get_query_pool_results(pool, 0, &mut stamps, vk::QueryResultFlags::TYPE_64)
                .ok()?;
        }
        let ticks = stamps[1].wrapping_sub(stamps[0]);
        Some(ticks as f32 * gpu.properties.limits.timestamp_period / 1e6)
    }

    pub fn destroy(&mut self, device: &ash::Device, allocator: &SharedAllocator) {
        unsafe {
            if let Some(q) = self.queries {
                device.destroy_query_pool(q, None);
            }
            device.destroy_semaphore(self.image_available, None);
            device.destroy_fence(self.in_flight, None);
            device.destroy_command_pool(self.pool, None);
        }
        self.uniforms.destroy(device, allocator);
    }
}
