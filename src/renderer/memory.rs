use std::sync::{Arc, Mutex};

use anyhow::Result;
use ash::vk;
use gpu_allocator::{
    vulkan::{Allocation, AllocationCreateDesc, AllocationScheme, Allocator},
    MemoryLocation,
};

pub type SharedAllocator = Arc<Mutex<Allocator>>;

pub struct Buffer {
    pub buffer: vk::Buffer,
    allocation: Option<Allocation>,
}

impl Buffer {
    pub fn new(
        device: &ash::Device,
        allocator: &SharedAllocator,
        name: &str,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
    ) -> Result<Self> {
        let info = vk::BufferCreateInfo::default()
            .size(size.max(4))
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { device.create_buffer(&info, None)? };
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let allocation = allocator.lock().unwrap().allocate(&AllocationCreateDesc {
            name,
            requirements,
            location,
            linear: true,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        });
        let allocation = match allocation {
            Ok(a) => a,
            Err(e) => {
                unsafe { device.destroy_buffer(buffer, None) };
                return Err(e.into());
            }
        };
        unsafe { device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset())? };
        Ok(Buffer {
            buffer,
            allocation: Some(allocation),
        })
    }

    /// Host-visible buffers are persistently mapped by gpu-allocator.
    pub fn mapped(&mut self) -> &mut [u8] {
        self.allocation
            .as_mut()
            .and_then(|a| a.mapped_slice_mut())
            .expect("buffer is not host visible")
    }

    pub fn write<T: bytemuck::Pod>(&mut self, offset: usize, data: &[T]) {
        let bytes = bytemuck::cast_slice(data);
        self.mapped()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    pub fn destroy(&mut self, device: &ash::Device, allocator: &SharedAllocator) {
        if let Some(allocation) = self.allocation.take() {
            unsafe { device.destroy_buffer(self.buffer, None) };
            allocator.lock().unwrap().free(allocation).ok();
        }
    }
}

pub struct Image {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub extent: vk::Extent2D,
    pub mip_levels: u32,
    allocation: Option<Allocation>,
}

pub struct ImageDesc {
    pub name: &'static str,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    pub usage: vk::ImageUsageFlags,
    pub aspect: vk::ImageAspectFlags,
    pub samples: vk::SampleCountFlags,
    pub mip_levels: u32,
}

impl Image {
    pub fn new(
        device: &ash::Device,
        allocator: &SharedAllocator,
        desc: &ImageDesc,
    ) -> Result<Self> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(desc.format)
            .extent(vk::Extent3D {
                width: desc.extent.width,
                height: desc.extent.height,
                depth: 1,
            })
            .mip_levels(desc.mip_levels)
            .array_layers(1)
            .samples(desc.samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(desc.usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { device.create_image(&info, None)? };
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let allocation = allocator.lock().unwrap().allocate(&AllocationCreateDesc {
            name: desc.name,
            requirements,
            location: MemoryLocation::GpuOnly,
            linear: false,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        });
        let allocation = match allocation {
            Ok(a) => a,
            Err(e) => {
                unsafe { device.destroy_image(image, None) };
                return Err(e.into());
            }
        };
        unsafe { device.bind_image_memory(image, allocation.memory(), allocation.offset())? };

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(desc.format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: desc.aspect,
                base_mip_level: 0,
                level_count: desc.mip_levels,
                base_array_layer: 0,
                layer_count: 1,
            });
        let view = unsafe { device.create_image_view(&view_info, None)? };

        Ok(Image {
            image,
            view,
            extent: desc.extent,
            mip_levels: desc.mip_levels,
            allocation: Some(allocation),
        })
    }

    pub fn destroy(&mut self, device: &ash::Device, allocator: &SharedAllocator) {
        if let Some(allocation) = self.allocation.take() {
            unsafe {
                device.destroy_image_view(self.view, None);
                device.destroy_image(self.image, None);
            }
            allocator.lock().unwrap().free(allocation).ok();
        }
    }
}
