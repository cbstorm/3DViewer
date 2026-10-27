use anyhow::Result;
use ash::vk;

use super::{
    device::Gpu,
    memory::{Image, ImageDesc, SharedAllocator},
};

pub const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

pub struct Swapchain {
    pub handle: vk::SwapchainKHR,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    pub extent: vk::Extent2D,
    /// Images can be copied out (for screenshots).
    pub transfer_src: bool,
    /// Signalled when rendering to image `i` is done; one per image so a
    /// semaphore is never re-signalled while presentation may still wait on it.
    pub render_finished: Vec<vk::Semaphore>,
}

/// Prefer an sRGB format so shaders can output linear color.
pub fn choose_surface_format(gpu: &Gpu) -> Result<vk::SurfaceFormatKHR> {
    let formats = unsafe {
        gpu.surface_loader
            .get_physical_device_surface_formats(gpu.physical_device, gpu.surface)?
    };
    let preferred = [vk::Format::B8G8R8A8_SRGB, vk::Format::R8G8B8A8_SRGB];
    Ok(preferred
        .iter()
        .find_map(|&f| {
            formats
                .iter()
                .find(|s| s.format == f && s.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        })
        .copied()
        .unwrap_or(formats[0]))
}

impl Swapchain {
    pub fn new(
        gpu: &Gpu,
        format: vk::SurfaceFormatKHR,
        window_size: (u32, u32),
        vsync: bool,
        old: vk::SwapchainKHR,
    ) -> Result<Self> {
        let caps = unsafe {
            gpu.surface_loader
                .get_physical_device_surface_capabilities(gpu.physical_device, gpu.surface)?
        };
        let modes = unsafe {
            gpu.surface_loader
                .get_physical_device_surface_present_modes(gpu.physical_device, gpu.surface)?
        };
        let present_mode = if vsync {
            vk::PresentModeKHR::FIFO
        } else {
            [vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::IMMEDIATE]
                .into_iter()
                .find(|m| modes.contains(m))
                .unwrap_or(vk::PresentModeKHR::FIFO)
        };

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: window_size
                    .0
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: window_size
                    .1
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            image_count = image_count.min(caps.max_image_count);
        }
        let composite_alpha = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
        .into_iter()
        .find(|&a| caps.supported_composite_alpha.contains(a))
        .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);

        let transfer_src = caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_SRC);
        let mut usage = vk::ImageUsageFlags::COLOR_ATTACHMENT;
        if transfer_src {
            usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(gpu.surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(usage)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(composite_alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(old);
        let handle = unsafe { gpu.swapchain_loader.create_swapchain(&info, None)? };
        let images = unsafe { gpu.swapchain_loader.get_swapchain_images(handle)? };

        let mut views = Vec::with_capacity(images.len());
        let mut render_finished = Vec::with_capacity(images.len());
        for &image in &images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format.format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            views.push(unsafe { gpu.device.create_image_view(&view_info, None)? });
            render_finished.push(unsafe {
                gpu.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?
            });
        }
        tracing::debug!(
            "swapchain {}x{}, {} images, {:?}",
            extent.width,
            extent.height,
            images.len(),
            present_mode
        );

        Ok(Swapchain {
            handle,
            images,
            views,
            extent,
            transfer_src,
            render_finished,
        })
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        unsafe {
            for &view in &self.views {
                gpu.device.destroy_image_view(view, None);
            }
            for &s in &self.render_finished {
                gpu.device.destroy_semaphore(s, None);
            }
            gpu.swapchain_loader.destroy_swapchain(self.handle, None);
        }
        self.views.clear();
        self.render_finished.clear();
    }
}

/// Size-dependent attachments: the depth buffer and, with MSAA, the
/// multisampled color image that resolves into the swapchain image.
pub struct RenderTargets {
    pub depth: Image,
    pub msaa_color: Option<Image>,
}

impl RenderTargets {
    pub fn new(
        device: &ash::Device,
        allocator: &SharedAllocator,
        extent: vk::Extent2D,
        color_format: vk::Format,
        samples: vk::SampleCountFlags,
    ) -> Result<Self> {
        let depth = Image::new(
            device,
            allocator,
            &ImageDesc {
                name: "depth",
                extent,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT,
                aspect: vk::ImageAspectFlags::DEPTH,
                samples,
                mip_levels: 1,
            },
        )?;
        let msaa_color = if samples != vk::SampleCountFlags::TYPE_1 {
            Some(Image::new(
                device,
                allocator,
                &ImageDesc {
                    name: "msaa color",
                    extent,
                    format: color_format,
                    usage: vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT,
                    aspect: vk::ImageAspectFlags::COLOR,
                    samples,
                    mip_levels: 1,
                },
            )?)
        } else {
            None
        };
        Ok(RenderTargets { depth, msaa_color })
    }

    pub fn destroy(&mut self, device: &ash::Device, allocator: &SharedAllocator) {
        self.depth.destroy(device, allocator);
        if let Some(img) = &mut self.msaa_color {
            img.destroy(device, allocator);
        }
    }
}
