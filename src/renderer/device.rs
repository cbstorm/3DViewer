use std::ffi::{c_char, c_void, CStr};

use anyhow::{anyhow, bail, Context, Result};
use ash::{ext, khr, vk};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

/// `vkCmdBeginRendering` comes from core 1.3 or `VK_KHR_dynamic_rendering`
/// (the latter is what MoltenVK exposes on older SDKs).
pub enum DynamicRendering {
    Core,
    Khr(khr::dynamic_rendering::Device),
}

/// Instance, device, queue, and surface: everything that lives as long as the window.
pub struct Gpu {
    _entry: ash::Entry,
    pub instance: ash::Instance,
    debug: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    pub surface_loader: khr::surface::Instance,
    pub surface: vk::SurfaceKHR,
    pub physical_device: vk::PhysicalDevice,
    pub properties: vk::PhysicalDeviceProperties,
    pub device: ash::Device,
    pub queue_family: u32,
    pub queue: vk::Queue,
    pub swapchain_loader: khr::swapchain::Device,
    pub dynamic_rendering: DynamicRendering,
    pub supports_wireframe: bool,
    pub supports_anisotropy: bool,
    pub timestamps: bool,
}

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";
const PORTABILITY_SUBSET: &CStr = c"VK_KHR_portability_subset";

impl Gpu {
    pub fn new(window: &Window) -> Result<Self> {
        let entry = load_vulkan()?;
        let display = window.display_handle()?.as_raw();

        let instance_version =
            unsafe { entry.try_enumerate_instance_version()? }.unwrap_or(vk::API_VERSION_1_0);
        if instance_version < vk::API_VERSION_1_2 {
            bail!(
                "Vulkan 1.2 or newer is required (loader reports 1.{})",
                vk::api_version_minor(instance_version)
            );
        }
        let api_version = instance_version.min(vk::API_VERSION_1_3);

        let available_layers = unsafe { entry.enumerate_instance_layer_properties()? };
        let available_exts = unsafe { entry.enumerate_instance_extension_properties(None)? };
        let has_ext = |name: &CStr| {
            available_exts
                .iter()
                .any(|e| e.extension_name_as_c_str() == Ok(name))
        };

        // Validation is on in debug builds when the layer is installed, or on request.
        let want_validation =
            cfg!(debug_assertions) || std::env::var_os("VIEWER_VALIDATION").is_some();
        let validation = want_validation
            && available_layers
                .iter()
                .any(|l| l.layer_name_as_c_str() == Ok(VALIDATION_LAYER));
        if want_validation && !validation {
            tracing::info!("Vulkan validation layer not installed; running without it");
        }

        let mut extensions: Vec<*const c_char> =
            ash_window::enumerate_required_extensions(display)?.to_vec();
        let debug_utils = has_ext(ext::debug_utils::NAME);
        if debug_utils {
            extensions.push(ext::debug_utils::NAME.as_ptr());
        }
        let mut flags = vk::InstanceCreateFlags::empty();
        if has_ext(khr::portability_enumeration::NAME) {
            extensions.push(khr::portability_enumeration::NAME.as_ptr());
            flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }
        let layers: Vec<*const c_char> = if validation {
            vec![VALIDATION_LAYER.as_ptr()]
        } else {
            vec![]
        };

        let app_info = vk::ApplicationInfo::default()
            .application_name(c"3D Viewer")
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(c"viewer3d")
            .api_version(api_version);
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&extensions)
            .enabled_layer_names(&layers)
            .flags(flags);
        let instance = unsafe { entry.create_instance(&instance_info, None)? };

        let debug = if debug_utils && validation {
            let loader = ext::debug_utils::Instance::new(&entry, &instance);
            let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                        | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(debug_callback));
            let messenger = unsafe { loader.create_debug_utils_messenger(&info, None)? };
            Some((loader, messenger))
        } else {
            None
        };

        let surface_loader = khr::surface::Instance::new(&entry, &instance);
        let surface = unsafe {
            ash_window::create_surface(
                &entry,
                &instance,
                display,
                window.window_handle()?.as_raw(),
                None,
            )?
        };

        let (physical_device, queue_family) =
            pick_physical_device(&instance, &surface_loader, surface)?;
        let properties = unsafe { instance.get_physical_device_properties(physical_device) };
        let features = unsafe { instance.get_physical_device_features(physical_device) };
        let device_name = properties
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        tracing::info!(
            "using {device_name} (Vulkan {}.{}.{})",
            vk::api_version_major(properties.api_version),
            vk::api_version_minor(properties.api_version),
            vk::api_version_patch(properties.api_version)
        );

        let device_exts =
            unsafe { instance.enumerate_device_extension_properties(physical_device)? };
        let device_has = |name: &CStr| {
            device_exts
                .iter()
                .any(|e| e.extension_name_as_c_str() == Ok(name))
        };
        let core_dynamic_rendering =
            api_version >= vk::API_VERSION_1_3 && properties.api_version >= vk::API_VERSION_1_3;

        let mut device_extensions = vec![khr::swapchain::NAME.as_ptr()];
        if !core_dynamic_rendering {
            device_extensions.push(khr::dynamic_rendering::NAME.as_ptr());
        }
        if device_has(PORTABILITY_SUBSET) {
            device_extensions.push(PORTABILITY_SUBSET.as_ptr());
        }

        let enabled_features = vk::PhysicalDeviceFeatures::default()
            .fill_mode_non_solid(features.fill_mode_non_solid == vk::TRUE)
            .sampler_anisotropy(features.sampler_anisotropy == vk::TRUE);
        let mut vk13 = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true);
        let mut khr_dr =
            vk::PhysicalDeviceDynamicRenderingFeaturesKHR::default().dynamic_rendering(true);

        let priorities = [1.0];
        let queue_infos = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let mut device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_infos)
            .enabled_extension_names(&device_extensions)
            .enabled_features(&enabled_features);
        device_info = if core_dynamic_rendering {
            device_info.push_next(&mut vk13)
        } else {
            device_info.push_next(&mut khr_dr)
        };
        let device = unsafe { instance.create_device(physical_device, &device_info, None)? };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        let dynamic_rendering = if core_dynamic_rendering {
            DynamicRendering::Core
        } else {
            DynamicRendering::Khr(khr::dynamic_rendering::Device::new(&instance, &device))
        };

        let queue_props =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let timestamps = queue_props[queue_family as usize].timestamp_valid_bits > 0
            && properties.limits.timestamp_period > 0.0;

        Ok(Gpu {
            swapchain_loader: khr::swapchain::Device::new(&instance, &device),
            _entry: entry,
            instance,
            debug,
            surface_loader,
            surface,
            physical_device,
            properties,
            device,
            queue_family,
            queue,
            dynamic_rendering,
            supports_wireframe: features.fill_mode_non_solid == vk::TRUE,
            supports_anisotropy: features.sampler_anisotropy == vk::TRUE,
            timestamps,
        })
    }

    pub fn device_name(&self) -> String {
        self.properties
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn cmd_begin_rendering(&self, cmd: vk::CommandBuffer, info: &vk::RenderingInfo) {
        unsafe {
            match &self.dynamic_rendering {
                DynamicRendering::Core => self.device.cmd_begin_rendering(cmd, info),
                DynamicRendering::Khr(ext) => ext.cmd_begin_rendering(cmd, info),
            }
        }
    }

    pub fn cmd_end_rendering(&self, cmd: vk::CommandBuffer) {
        unsafe {
            match &self.dynamic_rendering {
                DynamicRendering::Core => self.device.cmd_end_rendering(cmd),
                DynamicRendering::Khr(ext) => ext.cmd_end_rendering(cmd),
            }
        }
    }

    /// Highest sample count no greater than `wanted` usable for both color and depth.
    pub fn max_sample_count(&self, wanted: u32) -> vk::SampleCountFlags {
        let limits = &self.properties.limits;
        let supported =
            limits.framebuffer_color_sample_counts & limits.framebuffer_depth_sample_counts;
        [
            vk::SampleCountFlags::TYPE_8,
            vk::SampleCountFlags::TYPE_4,
            vk::SampleCountFlags::TYPE_2,
        ]
        .into_iter()
        .find(|&s| s.as_raw() <= wanted && supported.contains(s))
        .unwrap_or(vk::SampleCountFlags::TYPE_1)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            if let Some((loader, messenger)) = self.debug.take() {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

fn load_vulkan() -> Result<ash::Entry> {
    #[cfg(target_os = "macos")]
    if let Some(entry) = bundled_vulkan() {
        return Ok(entry);
    }
    unsafe { ash::Entry::load() }
        .context("could not load the Vulkan loader; is a Vulkan driver installed?")
}

/// Inside a macOS .app bundle, the Vulkan loader or MoltenVK lives in
/// `Contents/Frameworks`, which the default library search doesn't cover.
#[cfg(target_os = "macos")]
fn bundled_vulkan() -> Option<ash::Entry> {
    let frameworks = std::env::current_exe()
        .ok()?
        .parent()?
        .join("../Frameworks");
    ["libvulkan.1.dylib", "libMoltenVK.dylib"]
        .iter()
        .map(|name| frameworks.join(name))
        .filter(|path| path.exists())
        .find_map(|path| unsafe { ash::Entry::load_from(&path) }.ok())
}

fn pick_physical_device(
    instance: &ash::Instance,
    surface_loader: &khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32)> {
    let devices = unsafe { instance.enumerate_physical_devices()? };
    let mut best = None;
    for pd in devices {
        let props = unsafe { instance.get_physical_device_properties(pd) };
        if props.api_version < vk::API_VERSION_1_2 {
            continue;
        }
        let exts = unsafe { instance.enumerate_device_extension_properties(pd)? };
        let has = |name: &CStr| exts.iter().any(|e| e.extension_name_as_c_str() == Ok(name));
        if !has(khr::swapchain::NAME)
            || (props.api_version < vk::API_VERSION_1_3 && !has(khr::dynamic_rendering::NAME))
        {
            continue;
        }

        let families = unsafe { instance.get_physical_device_queue_family_properties(pd) };
        let family = families.iter().enumerate().find_map(|(i, f)| {
            let present = unsafe {
                surface_loader
                    .get_physical_device_surface_support(pd, i as u32, surface)
                    .unwrap_or(false)
            };
            (f.queue_flags.contains(vk::QueueFlags::GRAPHICS) && present).then_some(i as u32)
        });
        let Some(family) = family else { continue };

        let score = match props.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
            _ => 0,
        };
        if best.is_none_or(|(s, _, _)| score > s) {
            best = Some((score, pd, family));
        }
    }
    best.map(|(_, pd, family)| (pd, family)).ok_or_else(|| {
        anyhow!("no Vulkan GPU with graphics, presentation and dynamic rendering support found")
    })
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut c_void,
) -> vk::Bool32 {
    let message = unsafe { data.as_ref() }
        .and_then(|d| unsafe { d.message_as_c_str() })
        .map(|m| m.to_string_lossy())
        .unwrap_or_default();
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        tracing::error!(target: "vulkan", "{message}");
    } else {
        tracing::warn!(target: "vulkan", "{message}");
    }
    vk::FALSE
}
