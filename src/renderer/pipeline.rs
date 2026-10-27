use std::io::Cursor;

use anyhow::Result;
use ash::vk;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec4};

use super::{device::Gpu, swapchain::DEPTH_FORMAT};
use crate::scene::Vertex;

macro_rules! spirv {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".spv"))
    };
}

/// Push constants for mesh draws. Must match `Push` in mesh.wgsl.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct MeshPush {
    pub model: Mat4,
    pub base_color: Vec4,
    pub emissive: Vec4,
    /// x: metallic, y: roughness, z: mode, w: alpha cutoff
    pub params: Vec4,
}

/// Push constants for line draws. Must match `Push` in lines.wgsl.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct LinePush {
    pub model: Mat4,
    pub color: Vec4,
    pub params: Vec4,
}

pub const MODE_LIT: f32 = 0.0;
pub const MODE_NORMALS: f32 = 1.0;
pub const MODE_FLAT: f32 = 2.0;

pub struct Pipelines {
    pub mesh_layout: vk::PipelineLayout,
    pub lines_layout: vk::PipelineLayout,
    pub opaque: vk::Pipeline,
    pub blend: vk::Pipeline,
    /// Needs `fillModeNonSolid`.
    pub wireframe: Option<vk::Pipeline>,
    pub bounds: vk::Pipeline,
    pub grid: vk::Pipeline,
}

struct PipelineDesc<'a> {
    layout: vk::PipelineLayout,
    vs: vk::ShaderModule,
    vs_entry: &'a std::ffi::CStr,
    fs: vk::ShaderModule,
    topology: vk::PrimitiveTopology,
    polygon_mode: vk::PolygonMode,
    mesh_vertices: bool,
    depth_write: bool,
    blend: bool,
    depth_bias: bool,
}

impl Pipelines {
    pub fn new(
        gpu: &Gpu,
        frame_layout: vk::DescriptorSetLayout,
        material_layout: vk::DescriptorSetLayout,
        color_format: vk::Format,
        samples: vk::SampleCountFlags,
    ) -> Result<Self> {
        let device = &gpu.device;
        let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;

        let mesh_push = [vk::PushConstantRange {
            stage_flags: stages,
            offset: 0,
            size: std::mem::size_of::<MeshPush>() as u32,
        }];
        let mesh_sets = [frame_layout, material_layout];
        let mesh_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&mesh_sets)
                    .push_constant_ranges(&mesh_push),
                None,
            )?
        };
        let line_push = [vk::PushConstantRange {
            stage_flags: stages,
            offset: 0,
            size: std::mem::size_of::<LinePush>() as u32,
        }];
        let line_sets = [frame_layout];
        let lines_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&line_sets)
                    .push_constant_ranges(&line_push),
                None,
            )?
        };

        let modules = [
            spirv!("mesh.vs_main").as_slice(),
            spirv!("mesh.fs_main"),
            spirv!("lines.vs_bounds"),
            spirv!("lines.vs_grid"),
            spirv!("lines.fs_main"),
        ]
        .map(|bytes| create_shader_module(device, bytes));
        let [mesh_vs, mesh_fs, bounds_vs, grid_vs, lines_fs] = modules;
        let (mesh_vs, mesh_fs, bounds_vs, grid_vs, lines_fs) =
            (mesh_vs?, mesh_fs?, bounds_vs?, grid_vs?, lines_fs?);

        let build = |d: PipelineDesc| create_pipeline(device, &d, color_format, samples);
        let mesh = |polygon_mode, depth_write, blend, depth_bias| PipelineDesc {
            layout: mesh_layout,
            vs: mesh_vs,
            vs_entry: c"vs_main",
            fs: mesh_fs,
            topology: vk::PrimitiveTopology::TRIANGLE_LIST,
            polygon_mode,
            mesh_vertices: true,
            depth_write,
            blend,
            depth_bias,
        };
        let lines = |vs, vs_entry| PipelineDesc {
            layout: lines_layout,
            vs,
            vs_entry,
            fs: lines_fs,
            topology: vk::PrimitiveTopology::LINE_LIST,
            polygon_mode: vk::PolygonMode::FILL,
            mesh_vertices: false,
            depth_write: false,
            blend: true,
            depth_bias: false,
        };

        let pipelines = (|| -> Result<Self> {
            Ok(Pipelines {
                mesh_layout,
                lines_layout,
                opaque: build(mesh(vk::PolygonMode::FILL, true, false, false))?,
                blend: build(mesh(vk::PolygonMode::FILL, false, true, false))?,
                wireframe: if gpu.supports_wireframe {
                    Some(build(mesh(vk::PolygonMode::LINE, false, true, true))?)
                } else {
                    None
                },
                bounds: build(lines(bounds_vs, c"vs_bounds"))?,
                grid: build(lines(grid_vs, c"vs_grid"))?,
            })
        })();

        unsafe {
            for m in [mesh_vs, mesh_fs, bounds_vs, grid_vs, lines_fs] {
                device.destroy_shader_module(m, None);
            }
        }
        pipelines
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            for p in [
                Some(self.opaque),
                Some(self.blend),
                self.wireframe,
                Some(self.bounds),
                Some(self.grid),
            ]
            .into_iter()
            .flatten()
            {
                device.destroy_pipeline(p, None);
            }
            device.destroy_pipeline_layout(self.mesh_layout, None);
            device.destroy_pipeline_layout(self.lines_layout, None);
        }
    }
}

fn create_shader_module(device: &ash::Device, bytes: &[u8]) -> Result<vk::ShaderModule> {
    let code = ash::util::read_spv(&mut Cursor::new(bytes))?;
    Ok(unsafe {
        device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)?
    })
}

fn create_pipeline(
    device: &ash::Device,
    d: &PipelineDesc,
    color_format: vk::Format,
    samples: vk::SampleCountFlags,
) -> Result<vk::Pipeline> {
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(d.vs)
            .name(d.vs_entry),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(d.fs)
            .name(c"fs_main"),
    ];

    let bindings = [vk::VertexInputBindingDescription {
        binding: 0,
        stride: std::mem::size_of::<Vertex>() as u32,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attributes = [
        (0, vk::Format::R32G32B32_SFLOAT, 0),
        (1, vk::Format::R32G32B32_SFLOAT, 12),
        (2, vk::Format::R32G32_SFLOAT, 24),
    ]
    .map(
        |(location, format, offset)| vk::VertexInputAttributeDescription {
            location,
            binding: 0,
            format,
            offset,
        },
    );
    let vertex_input = if d.mesh_vertices {
        vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes)
    } else {
        vk::PipelineVertexInputStateCreateInfo::default()
    };

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(d.topology);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(d.polygon_mode)
        // Many models have inconsistent winding; shading is two-sided instead.
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0)
        .depth_bias_enable(d.depth_bias)
        .depth_bias_constant_factor(1.0)
        .depth_bias_slope_factor(1.0);
    let multisample =
        vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(samples);
    // Reverse Z: larger depth is closer.
    let depth = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(d.depth_write)
        .depth_compare_op(vk::CompareOp::GREATER_OR_EQUAL);
    let blend_attachment = [if d.blend {
        vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)
    } else {
        vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
    }];
    let color_blend =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_formats = [color_format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_formats)
        .depth_attachment_format(DEPTH_FORMAT);

    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport)
        .rasterization_state(&rasterization)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&color_blend)
        .dynamic_state(&dynamic)
        .layout(d.layout)
        .push_next(&mut rendering);
    let pipelines = unsafe {
        device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
            .map_err(|(_, e)| e)?
    };
    Ok(pipelines[0])
}
