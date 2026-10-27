mod descriptors;
mod device;
mod frame;
mod gpu_scene;
mod memory;
mod pipeline;
mod swapchain;
mod upload;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ash::vk;
use egui_ash_renderer::{allocator::GpuAllocator, DynamicRendering, Options, RenderMode};
use glam::{Mat4, Vec3, Vec4};
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use winit::window::Window;

use crate::scene::{Aabb, AlphaMode, Frustum, OrbitCamera, Scene};
use descriptors::{Descriptors, FrameUniforms};
use device::Gpu;
use frame::{FrameResources, FRAMES_IN_FLIGHT};
use gpu_scene::{DefaultTextures, GpuScene};
use memory::SharedAllocator;
use pipeline::{LinePush, MeshPush, Pipelines, MODE_FLAT, MODE_LIT, MODE_NORMALS};
use swapchain::{RenderTargets, Swapchain};
use upload::Uploader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Shaded,
    ShadedWireframe,
    Wireframe,
    Normals,
}

impl ViewMode {
    pub const ALL: [ViewMode; 4] = [
        ViewMode::Shaded,
        ViewMode::ShadedWireframe,
        ViewMode::Wireframe,
        ViewMode::Normals,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Shaded => "Shaded",
            ViewMode::ShadedWireframe => "Shaded + wireframe",
            ViewMode::Wireframe => "Wireframe",
            ViewMode::Normals => "Normals",
        }
    }

    pub fn needs_wireframe(self) -> bool {
        matches!(self, ViewMode::ShadedWireframe | ViewMode::Wireframe)
    }
}

#[derive(Debug, Clone)]
pub struct ViewSettings {
    pub mode: ViewMode,
    pub show_grid: bool,
    pub show_bounds: bool,
    /// sRGB.
    pub background: [u8; 3],
    pub light_intensity: f32,
    pub ambient: f32,
    /// Light direction relative to the camera, in degrees.
    pub light_azimuth: f32,
    pub light_elevation: f32,
    pub frustum_culling: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            mode: ViewMode::Shaded,
            show_grid: true,
            show_bounds: false,
            background: [38, 40, 46],
            light_intensity: 3.0,
            ambient: 0.35,
            light_azimuth: 35.0,
            light_elevation: 40.0,
            frustum_culling: true,
        }
    }
}

pub struct EguiFrame {
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
    pub pixels_per_point: f32,
}

pub struct FrameInput<'a> {
    pub scene: Option<&'a Scene>,
    pub camera: &'a OrbitCamera,
    pub settings: &'a ViewSettings,
    pub selected_node: Option<usize>,
    /// 3D viewport in physical pixels: x, y, width, height.
    pub viewport: [f32; 4],
    pub egui: EguiFrame,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub draw_calls: u32,
    pub triangles: u64,
    pub culled: u32,
    pub gpu_ms: Option<f32>,
    /// Time the CPU spent blocked on the GPU or the presentation engine.
    pub wait_ms: f32,
}

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub msaa_samples: u32,
    pub wireframe: bool,
    pub scene_memory_bytes: u64,
}

struct DrawItem {
    model: Mat4,
    mesh: usize,
    primitive: usize,
    material: usize,
    /// Squared distance to the camera, for sorting blended draws.
    depth: f32,
}

pub struct Renderer {
    // Declaration order matters for drop: everything holding the allocator or
    // device goes first; `allocator` is dropped before `gpu` destroys the device.
    egui: Option<egui_ash_renderer::Renderer<GpuAllocator>>,
    gpu_scene: Option<GpuScene>,
    defaults: DefaultTextures,
    frames: Vec<FrameResources>,
    frame_index: usize,
    pipelines: Pipelines,
    descriptors: Descriptors,
    targets: RenderTargets,
    swapchain: Swapchain,
    surface_format: vk::SurfaceFormatKHR,
    samples: vk::SampleCountFlags,
    uploader: Uploader,
    vsync: bool,
    needs_recreate: bool,
    last_gpu_ms: Option<f32>,
    capture_requested: bool,
    captured: Option<Result<image::RgbaImage, String>>,
    /// egui texture changes not yet applied; survives skipped frames.
    pending_textures: egui::TexturesDelta,
    allocator: SharedAllocator,
    gpu: Gpu,
}

impl Renderer {
    pub fn new(window: &Window, vsync: bool) -> Result<Self> {
        let gpu = Gpu::new(window)?;
        let allocator = Arc::new(Mutex::new(Allocator::new(&AllocatorCreateDesc {
            instance: gpu.instance.clone(),
            device: gpu.device.clone(),
            physical_device: gpu.physical_device,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })?));

        let surface_format = swapchain::choose_surface_format(&gpu)?;
        let samples = gpu.max_sample_count(4);
        let size = window.inner_size();
        let swapchain = Swapchain::new(
            &gpu,
            surface_format,
            (size.width, size.height),
            vsync,
            vk::SwapchainKHR::null(),
        )?;
        let targets = RenderTargets::new(
            &gpu.device,
            &allocator,
            swapchain.extent,
            surface_format.format,
            samples,
        )?;
        let descriptors = Descriptors::new(&gpu, FRAMES_IN_FLIGHT as u32)?;
        let pipelines = Pipelines::new(
            &gpu,
            descriptors.frame_layout,
            descriptors.material_layout,
            surface_format.format,
            samples,
        )?;
        let frames = (0..FRAMES_IN_FLIGHT)
            .map(|_| FrameResources::new(&gpu, &allocator, &descriptors))
            .collect::<Result<Vec<_>>>()?;
        let uploader = Uploader::new(&gpu)?;
        let defaults = DefaultTextures::new(&gpu, &allocator, &uploader)?;

        let egui = egui_ash_renderer::Renderer::with_gpu_allocator(
            allocator.clone(),
            gpu.device.clone(),
            RenderMode::DynamicRendering(DynamicRendering {
                color_attachment_format: surface_format.format,
                depth_attachment_format: None,
                stencil_attachment_format: None,
            }),
            Options {
                in_flight_frames: FRAMES_IN_FLIGHT,
                srgb_framebuffer: true,
                ..Default::default()
            },
        )
        .context("creating egui renderer")?;

        Ok(Renderer {
            egui: Some(egui),
            gpu_scene: None,
            defaults,
            frames,
            frame_index: 0,
            pipelines,
            descriptors,
            targets,
            swapchain,
            surface_format,
            samples,
            uploader,
            vsync,
            needs_recreate: false,
            last_gpu_ms: None,
            capture_requested: false,
            captured: None,
            pending_textures: Default::default(),
            allocator,
            gpu,
        })
    }

    pub fn info(&self) -> GpuInfo {
        GpuInfo {
            name: self.gpu.device_name(),
            msaa_samples: self.samples.as_raw(),
            wireframe: self.pipelines.wireframe.is_some(),
            scene_memory_bytes: self.gpu_scene.as_ref().map_or(0, |s| s.memory_bytes),
        }
    }

    pub fn max_texture_side(&self) -> usize {
        self.gpu.properties.limits.max_image_dimension2_d as usize
    }

    /// Copy the next presented frame back to the CPU; fetch it with `take_capture`.
    pub fn request_capture(&mut self) {
        self.capture_requested = true;
    }

    pub fn take_capture(&mut self) -> Option<Result<image::RgbaImage, String>> {
        self.captured.take()
    }

    pub fn request_resize(&mut self) {
        self.needs_recreate = true;
    }

    pub fn set_vsync(&mut self, vsync: bool) {
        if self.vsync != vsync {
            self.vsync = vsync;
            self.needs_recreate = true;
        }
    }

    /// Replace the GPU copy of the scene. Blocks until the upload completes.
    pub fn upload_scene(&mut self, scene: &Scene) -> Result<()> {
        unsafe { self.gpu.device.device_wait_idle()? };
        if let Some(mut old) = self.gpu_scene.take() {
            old.destroy(&self.gpu.device, &self.allocator);
        }
        self.gpu_scene = Some(GpuScene::upload(
            &self.gpu,
            &self.allocator,
            &self.uploader,
            &self.descriptors,
            &self.defaults,
            scene,
        )?);
        Ok(())
    }

    fn recreate_swapchain(&mut self, window_size: (u32, u32)) -> Result<()> {
        unsafe { self.gpu.device.device_wait_idle()? };
        let new = Swapchain::new(
            &self.gpu,
            self.surface_format,
            window_size,
            self.vsync,
            self.swapchain.handle,
        )?;
        let mut old = std::mem::replace(&mut self.swapchain, new);
        old.destroy(&self.gpu);
        self.targets.destroy(&self.gpu.device, &self.allocator);
        self.targets = RenderTargets::new(
            &self.gpu.device,
            &self.allocator,
            self.swapchain.extent,
            self.surface_format.format,
            self.samples,
        )?;
        self.needs_recreate = false;
        Ok(())
    }

    /// Render and present one frame. Returns `None` if the frame was skipped
    /// (minimized window or swapchain being rebuilt).
    pub fn render(
        &mut self,
        window_size: (u32, u32),
        mut input: FrameInput,
    ) -> Result<Option<FrameStats>> {
        self.pending_textures
            .append(std::mem::take(&mut input.egui.textures_delta));
        if window_size.0 == 0 || window_size.1 == 0 {
            return Ok(None);
        }
        let device = self.gpu.device.clone();

        let fi = self.frame_index;
        let wait_start = std::time::Instant::now();
        unsafe { device.wait_for_fences(&[self.frames[fi].in_flight], true, u64::MAX)? };
        if let Some(ms) = self.frames[fi].read_gpu_time(&self.gpu) {
            self.last_gpu_ms = Some(ms);
        }

        if self.needs_recreate
            || self.swapchain.extent.width != window_size.0
            || self.swapchain.extent.height != window_size.1
        {
            self.recreate_swapchain(window_size)?;
        }

        let acquired = unsafe {
            self.gpu.swapchain_loader.acquire_next_image(
                self.swapchain.handle,
                u64::MAX,
                self.frames[fi].image_available,
                vk::Fence::null(),
            )
        };
        let image_index = match acquired {
            Ok((index, suboptimal)) => {
                self.needs_recreate |= suboptimal;
                index
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.needs_recreate = true;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let wait_ms = wait_start.elapsed().as_secs_f32() * 1e3;

        // egui texture uploads are synchronous and must precede recording.
        let egui = self.egui.as_mut().unwrap();
        for (id, deltas) in self.pending_textures.set.drain() {
            for delta in &deltas {
                egui.set_texture(self.gpu.queue, self.uploader.pool, id, delta)?;
            }
        }

        let frame = &mut self.frames[fi];
        let cmd = frame.cmd;
        unsafe {
            device.reset_fences(&[frame.in_flight])?;
            device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            if let Some(q) = frame.queries {
                device.cmd_reset_query_pool(cmd, q, 0, 2);
                device.cmd_write_timestamp(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, q, 0);
            }
        }

        let extent = self.swapchain.extent;
        let [vx, vy, vw, vh] = clamp_viewport(input.viewport, extent);
        let aspect = vw / vh;
        let camera = input.camera;
        let view_proj = camera.projection(aspect) * camera.view();
        frame
            .uniforms
            .write(0, &[frame_uniforms(camera, input.settings, view_proj)]);

        let swap_image = self.swapchain.images[image_index as usize];
        let swap_view = self.swapchain.views[image_index as usize];
        self.record_layout_transitions_begin(cmd, swap_image);

        // Scene pass (multisampled, resolved into the swapchain image).
        let bg = srgb_to_linear(input.settings.background);
        let clear_color = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [bg.x, bg.y, bg.z, 1.0],
            },
        };
        let color_attachment = match &self.targets.msaa_color {
            Some(msaa) => vk::RenderingAttachmentInfo::default()
                .image_view(msaa.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .resolve_mode(vk::ResolveModeFlags::AVERAGE)
                .resolve_image_view(swap_view)
                .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(clear_color),
            None => vk::RenderingAttachmentInfo::default()
                .image_view(swap_view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(clear_color),
        };
        let depth_attachment = vk::RenderingAttachmentInfo::default()
            .image_view(self.targets.depth.view)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .clear_value(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 0.0,
                    stencil: 0,
                },
            });
        let render_area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        let color_attachments = [color_attachment];
        self.gpu.cmd_begin_rendering(
            cmd,
            &vk::RenderingInfo::default()
                .render_area(render_area)
                .layer_count(1)
                .color_attachments(&color_attachments)
                .depth_attachment(&depth_attachment),
        );
        unsafe {
            // Negative height flips Y so clip space matches the usual Y-up convention.
            device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: vx,
                    y: vy + vh,
                    width: vw,
                    height: -vh,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            let scissor = vk::Rect2D {
                offset: vk::Offset2D {
                    x: vx as i32,
                    y: vy as i32,
                },
                extent: vk::Extent2D {
                    width: vw as u32,
                    height: vh as u32,
                },
            };
            device.cmd_set_scissor(cmd, 0, &[scissor]);
        }
        let mut stats = self.record_scene(cmd, &input, view_proj);
        self.gpu.cmd_end_rendering(cmd);

        // UI pass, drawn straight onto the resolved swapchain image.
        unsafe {
            let barrier = vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[barrier],
                &[],
                &[],
            );
        }
        let ui_attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(swap_view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)];
        self.gpu.cmd_begin_rendering(
            cmd,
            &vk::RenderingInfo::default()
                .render_area(render_area)
                .layer_count(1)
                .color_attachments(&ui_attachment),
        );
        self.egui.as_mut().unwrap().cmd_draw(
            cmd,
            extent,
            input.egui.pixels_per_point,
            &input.egui.primitives,
        )?;
        self.gpu.cmd_end_rendering(cmd);

        let capture = if std::mem::take(&mut self.capture_requested) {
            if self.swapchain.transfer_src {
                Some(self.record_capture(cmd, swap_image)?)
            } else {
                self.captured = Some(Err(
                    "this window surface does not allow reading back rendered frames".into(),
                ));
                None
            }
        } else {
            None
        };
        let frame = &mut self.frames[fi];
        let present_from = if capture.is_some() {
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL
        } else {
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
        };
        unsafe {
            let to_present = vk::ImageMemoryBarrier::default()
                .image(swap_image)
                .old_layout(present_from)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::TRANSFER_READ,
                )
                .dst_access_mask(vk::AccessFlags::empty())
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(color_range());
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            if let Some(q) = frame.queries {
                device.cmd_write_timestamp(cmd, vk::PipelineStageFlags::BOTTOM_OF_PIPE, q, 1);
                frame.queries_pending = true;
            }
            device.end_command_buffer(cmd)?;

            let wait = [frame.image_available];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let signal = [self.swapchain.render_finished[image_index as usize]];
            let cmds = [cmd];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&wait)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(&signal);
            device.queue_submit(self.gpu.queue, &[submit], frame.in_flight)?;

            let swapchains = [self.swapchain.handle];
            let indices = [image_index];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&signal)
                .swapchains(&swapchains)
                .image_indices(&indices);
            match self
                .gpu
                .swapchain_loader
                .queue_present(self.gpu.queue, &present)
            {
                Ok(suboptimal) => self.needs_recreate |= suboptimal,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.needs_recreate = true,
                Err(e) => return Err(e.into()),
            }
        }

        if let Some(mut buffer) = capture {
            let result = self.finish_capture(&mut buffer, fi);
            buffer.destroy(&device, &self.allocator);
            self.captured = Some(result.map_err(|e| format!("{e:#}")));
        }

        // Freed egui textures may still be referenced by frames in flight.
        if !self.pending_textures.free.is_empty() {
            unsafe { device.device_wait_idle()? };
            let egui = self.egui.as_mut().unwrap();
            for id in self.pending_textures.free.drain() {
                egui.free_texture(id)?;
            }
        }

        self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
        stats.gpu_ms = self.last_gpu_ms;
        stats.wait_ms = wait_ms;
        Ok(Some(stats))
    }

    /// Copy the finished swapchain image into a host-visible buffer, leaving
    /// the image in TRANSFER_SRC_OPTIMAL.
    fn record_capture(&self, cmd: vk::CommandBuffer, image: vk::Image) -> Result<memory::Buffer> {
        let extent = self.swapchain.extent;
        let buffer = memory::Buffer::new(
            &self.gpu.device,
            &self.allocator,
            "screenshot",
            extent.width as u64 * extent.height as u64 * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
            gpu_allocator::MemoryLocation::GpuToCpu,
        )?;
        let device = &self.gpu.device;
        unsafe {
            let barrier = vk::ImageMemoryBarrier::default()
                .image(image)
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(color_range());
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                });
            device.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer.buffer,
                &[region],
            );
        }
        Ok(buffer)
    }

    fn finish_capture(
        &self,
        buffer: &mut memory::Buffer,
        frame: usize,
    ) -> Result<image::RgbaImage> {
        unsafe {
            self.gpu
                .device
                .wait_for_fences(&[self.frames[frame].in_flight], true, u64::MAX)?
        };
        let extent = self.swapchain.extent;
        let mut pixels = buffer.mapped()[..(extent.width * extent.height * 4) as usize].to_vec();
        if matches!(
            self.surface_format.format,
            vk::Format::B8G8R8A8_SRGB | vk::Format::B8G8R8A8_UNORM
        ) {
            pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|p| p.swap(0, 2));
        }
        pixels
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|p| p[3] = 255);
        image::RgbaImage::from_raw(extent.width, extent.height, pixels)
            .context("screenshot buffer size mismatch")
    }

    fn record_layout_transitions_begin(&self, cmd: vk::CommandBuffer, swap_image: vk::Image) {
        use vk::{AccessFlags as A, ImageLayout as L, PipelineStageFlags as S};
        let barrier = |image, aspect, new_layout, dst_access| {
            vk::ImageMemoryBarrier::default()
                .image(image)
                .old_layout(L::UNDEFINED)
                .new_layout(new_layout)
                .src_access_mask(A::empty())
                .dst_access_mask(dst_access)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    ..color_range()
                })
        };
        let color_access = A::COLOR_ATTACHMENT_WRITE | A::COLOR_ATTACHMENT_READ;
        let mut color = vec![barrier(
            swap_image,
            vk::ImageAspectFlags::COLOR,
            L::COLOR_ATTACHMENT_OPTIMAL,
            color_access,
        )];
        if let Some(msaa) = &self.targets.msaa_color {
            color.push(barrier(
                msaa.image,
                vk::ImageAspectFlags::COLOR,
                L::COLOR_ATTACHMENT_OPTIMAL,
                color_access,
            ));
        }
        let depth = [barrier(
            self.targets.depth.image,
            vk::ImageAspectFlags::DEPTH,
            L::DEPTH_ATTACHMENT_OPTIMAL,
            A::DEPTH_STENCIL_ATTACHMENT_READ | A::DEPTH_STENCIL_ATTACHMENT_WRITE,
        )];
        unsafe {
            // Attachments are shared between frames in flight; these barriers also
            // order this frame's writes after the previous frame's.
            self.gpu.device.cmd_pipeline_barrier(
                cmd,
                S::COLOR_ATTACHMENT_OUTPUT,
                S::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &color,
            );
            self.gpu.device.cmd_pipeline_barrier(
                cmd,
                S::EARLY_FRAGMENT_TESTS | S::LATE_FRAGMENT_TESTS,
                S::EARLY_FRAGMENT_TESTS | S::LATE_FRAGMENT_TESTS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &depth,
            );
        }
    }

    fn record_scene(
        &self,
        cmd: vk::CommandBuffer,
        input: &FrameInput,
        view_proj: Mat4,
    ) -> FrameStats {
        let device = &self.gpu.device;
        let frame = &self.frames[self.frame_index];
        let settings = input.settings;
        let mut stats = FrameStats::default();
        let bind_frame_set = |layout| unsafe {
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[frame.frame_set],
                &[],
            );
        };

        let scene_bounds = input.scene.map(|s| s.bounds());

        if settings.show_grid {
            let bounds = scene_bounds.unwrap_or(Aabb {
                min: Vec3::splat(-1.0),
                max: Vec3::splat(1.0),
            });
            const CELLS: u32 = 40;
            let step = 10f32.powf(bounds.radius().max(1e-6).log10().floor());
            let c = bounds.center();
            let origin = Vec3::new(
                (c.x / step).round() * step,
                bounds.min.y,
                (c.z / step).round() * step,
            );
            let push = LinePush {
                model: Mat4::from_translation(origin) * Mat4::from_scale(Vec3::splat(step)),
                color: Vec4::new(0.5, 0.5, 0.55, 0.35),
                params: Vec4::new(CELLS as f32, 0.0, 0.0, 0.0),
            };
            unsafe {
                device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipelines.grid);
                bind_frame_set(self.pipelines.lines_layout);
                self.push(cmd, self.pipelines.lines_layout, &push);
                device.cmd_draw(cmd, (CELLS + 1) * 4, 1, 0, 0);
            }
            stats.draw_calls += 1;
        }

        let (Some(scene), Some(gpu_scene)) = (input.scene, &self.gpu_scene) else {
            return stats;
        };

        // Build the draw list: frustum-cull each primitive's world-space bounds.
        let frustum = Frustum::from_view_proj(&view_proj);
        let eye = input.camera.eye();
        let mut opaque = Vec::new();
        let mut blended = Vec::new();
        for node in scene.nodes.iter().filter(|n| n.visible) {
            let model = scene.world_transform(node);
            for (pi, p) in scene.meshes[node.mesh].primitives.iter().enumerate() {
                let bounds = p.bounds.transformed(&model);
                if settings.frustum_culling && !frustum.intersects(&bounds) {
                    stats.culled += 1;
                    continue;
                }
                let item = DrawItem {
                    model,
                    mesh: node.mesh,
                    primitive: pi,
                    material: p.material,
                    depth: bounds.center().distance_squared(eye),
                };
                match scene.materials[p.material].alpha_mode {
                    AlphaMode::Blend if settings.mode != ViewMode::Normals => blended.push(item),
                    _ => opaque.push(item),
                }
            }
        }
        // Group by material to minimize descriptor binds; blend back to front.
        opaque.sort_unstable_by_key(|d| d.material);
        blended.sort_unstable_by(|a, b| b.depth.total_cmp(&a.depth));

        let layout = self.pipelines.mesh_layout;
        let draw_list =
            |items: &[DrawItem], mode: f32, flat_color: Vec4, stats: &mut FrameStats| {
                let mut bound_material = usize::MAX;
                for d in items {
                    let material = &scene.materials[d.material];
                    if d.material != bound_material {
                        unsafe {
                            device.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::GRAPHICS,
                                layout,
                                1,
                                &[gpu_scene.material_sets[d.material]],
                                &[],
                            );
                        }
                        bound_material = d.material;
                    }
                    let cutoff = match material.alpha_mode {
                        AlphaMode::Mask(c) => c,
                        _ => 0.0,
                    };
                    let push = MeshPush {
                        model: d.model,
                        base_color: if mode == MODE_FLAT {
                            flat_color
                        } else {
                            Vec4::from(material.base_color)
                        },
                        emissive: Vec3::from(material.emissive).extend(0.0),
                        params: Vec4::new(material.metallic, material.roughness, mode, cutoff),
                    };
                    let gp = gpu_scene.meshes[d.mesh][d.primitive];
                    unsafe {
                        self.push(cmd, layout, &push);
                        device.cmd_draw_indexed(
                            cmd,
                            gp.index_count,
                            1,
                            gp.first_index,
                            gp.vertex_offset,
                            0,
                        );
                    }
                    stats.draw_calls += 1;
                    stats.triangles += gp.index_count as u64 / 3;
                }
            };

        gpu_scene.bind_geometry(device, cmd);
        let mode = settings.mode;
        if mode != ViewMode::Wireframe {
            let shading = if mode == ViewMode::Normals {
                MODE_NORMALS
            } else {
                MODE_LIT
            };
            unsafe {
                device.cmd_bind_pipeline(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipelines.opaque,
                );
            }
            bind_frame_set(layout);
            draw_list(&opaque, shading, Vec4::ZERO, &mut stats);
            if !blended.is_empty() {
                unsafe {
                    device.cmd_bind_pipeline(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.pipelines.blend,
                    );
                }
                draw_list(&blended, shading, Vec4::ZERO, &mut stats);
            }
        }
        if let (true, Some(wire)) = (mode.needs_wireframe(), self.pipelines.wireframe) {
            let color = if mode == ViewMode::Wireframe {
                Vec4::new(0.85, 0.88, 0.95, 1.0)
            } else {
                Vec4::new(0.05, 0.05, 0.08, 0.45)
            };
            unsafe {
                device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, wire);
            }
            bind_frame_set(layout);
            draw_list(&opaque, MODE_FLAT, color, &mut stats);
            draw_list(&blended, MODE_FLAT, color, &mut stats);
        }

        // Bounding boxes: whole scene on request, selected node always.
        let mut boxes = Vec::new();
        if settings.show_bounds {
            if let Some(b) = scene_bounds {
                boxes.push((b, Vec4::new(0.9, 0.9, 0.3, 0.8)));
            }
        }
        if let Some(node) = input.selected_node.and_then(|i| scene.nodes.get(i)) {
            boxes.push((scene.node_bounds(node), Vec4::new(1.0, 0.55, 0.1, 1.0)));
        }
        if !boxes.is_empty() {
            unsafe {
                device.cmd_bind_pipeline(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipelines.bounds,
                );
            }
            bind_frame_set(self.pipelines.lines_layout);
            for (b, color) in boxes {
                let push = LinePush {
                    model: b.unit_cube_transform(),
                    color,
                    params: Vec4::ZERO,
                };
                unsafe {
                    self.push(cmd, self.pipelines.lines_layout, &push);
                    device.cmd_draw(cmd, 24, 1, 0, 0);
                }
                stats.draw_calls += 1;
            }
        }
        stats
    }

    fn push<T: bytemuck::Pod>(&self, cmd: vk::CommandBuffer, layout: vk::PipelineLayout, data: &T) {
        let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
        unsafe {
            self.gpu
                .device
                .cmd_push_constants(cmd, layout, stages, 0, bytemuck::bytes_of(data))
        };
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let device = &self.gpu.device;
        unsafe { device.device_wait_idle().ok() };
        self.pending_textures.clear();
        self.egui.take();
        if let Some(mut s) = self.gpu_scene.take() {
            s.destroy(device, &self.allocator);
        }
        self.defaults.destroy(device, &self.allocator);
        for f in &mut self.frames {
            f.destroy(device, &self.allocator);
        }
        self.pipelines.destroy(device);
        self.descriptors.destroy(device);
        self.targets.destroy(device, &self.allocator);
        self.swapchain.destroy(&self.gpu);
        self.uploader.destroy(device);
    }
}

/// Keep the viewport inside the swapchain image and at least one pixel in size.
fn clamp_viewport([x, y, w, h]: [f32; 4], extent: vk::Extent2D) -> [f32; 4] {
    let (ew, eh) = (extent.width as f32, extent.height as f32);
    let x = x.round().clamp(0.0, ew - 1.0);
    let y = y.round().clamp(0.0, eh - 1.0);
    let w = w.round().clamp(1.0, ew - x);
    let h = h.round().clamp(1.0, eh - y);
    [x, y, w, h]
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn srgb_to_linear(c: [u8; 3]) -> Vec3 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    Vec3::new(f(c[0]), f(c[1]), f(c[2]))
}

fn frame_uniforms(camera: &OrbitCamera, s: &ViewSettings, view_proj: Mat4) -> FrameUniforms {
    // The key light is attached to the camera so the model is lit from any angle.
    let (az, el) = (s.light_azimuth.to_radians(), s.light_elevation.to_radians());
    let local = Vec3::new(az.sin() * el.cos(), el.sin(), az.cos() * el.cos());
    let light_dir =
        (camera.right() * local.x + camera.up() * local.y - camera.forward() * local.z).normalize();
    FrameUniforms {
        view_proj,
        camera_pos: camera.eye().extend(1.0),
        light_dir: light_dir.extend(s.light_intensity),
        light_color: Vec4::new(1.0, 0.97, 0.92, 1.0),
        sky_color: Vec4::new(0.75, 0.82, 1.0, s.ambient),
        ground_color: Vec4::new(0.35, 0.3, 0.27, 1.0),
    }
}
