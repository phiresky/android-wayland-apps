//! Vulkan renderer for zero-copy dmabuf compositing on Android.
//!
//! Uses the proprietary Qualcomm Vulkan driver to import client dmabufs
//! (from Turnip/KGSL) and blit them onto AHardwareBuffer targets for
//! presentation via ASurfaceTransaction.

mod dmabuf_import;
mod ahb_target;
mod blit;

use ash::vk;
use ash::khr;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::io::RawFd;

use super::surface_transaction::HardwareBuffer;

/// Vulkan renderer state for compositing client surfaces onto Android windows.
pub struct VulkanRenderer {
    _entry: ash::Entry,
    pub(super) instance: ash::Instance,
    pub(super) physical_device: vk::PhysicalDevice,
    pub(super) device: ash::Device,
    pub(super) queue: vk::Queue,
    _queue_family_index: u32,
    pub(super) external_memory_fd_fn: khr::external_memory_fd::Device,
    pub(super) ahb_fn: ash::android::external_memory_android_hardware_buffer::Device,
    pub(super) external_fence_fd_fn: khr::external_fence_fd::Device,
    /// Persistent command pool (kept alive for cmd_buf lifetime).
    _cmd_pool: vk::CommandPool,
    /// Persistent command buffer, reused every frame.
    pub(super) cmd_buf: vk::CommandBuffer,
    /// Cached LINEAR staging image for BGRA->RGBA format conversion.
    pub(super) staging_cache: std::cell::RefCell<Option<StagingImage>>,
    /// Cache imported dmabufs by fd to avoid re-importing every frame.
    /// Clients reuse a small pool of ~3 buffers.
    pub(super) dmabuf_cache: std::cell::RefCell<HashMap<RawFd, ImportedDmabuf>>,
    /// Host-visible staging buffer for shm->AHB uploads.
    pub(super) shm_staging: std::cell::RefCell<Option<ShmStaging>>,
}

pub(super) struct ShmStaging {
    pub(super) buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
    pub(super) mapped: *mut u8,
    pub(super) size: u64,
}

pub(super) struct StagingImage {
    pub(super) image: vk::Image,
    pub(super) memory: vk::DeviceMemory,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// AHardwareBuffer-backed VkImage for ASurfaceTransaction presentation.
pub struct AhbTarget {
    pub ahb: HardwareBuffer,
    pub vk_image: vk::Image,
    pub vk_memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
}

/// An imported dmabuf as Vulkan resources.
pub struct ImportedDmabuf {
    pub image: vk::Image,
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub width: u32,
    pub height: u32,
    pub stride_pixels: u32,
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

impl VulkanRenderer {
    /// Create a new Vulkan renderer using the proprietary Qualcomm driver.
    pub fn new() -> Result<Self, String> {
        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| format!("Failed to load Vulkan: {e}"))?;

        let app_name = cstr("wayland-compositor");
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(1)
            .api_version(vk::make_api_version(0, 1, 1, 0));

        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info);

        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|e| format!("vkCreateInstance: {e}"))?;

        // Select physical device
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate_physical_devices: {e}"))?;
        let physical_device = physical_devices.into_iter().next()
            .ok_or("No Vulkan physical device")?;

        let props = unsafe { instance.get_physical_device_properties(physical_device) };
        let name = unsafe { std::ffi::CStr::from_ptr(props.device_name.as_ptr()) };
        tracing::info!("[vk-renderer] GPU: {:?}", name);

        // Find graphics queue family
        let queue_families = unsafe {
            instance.get_physical_device_queue_family_properties(physical_device)
        };
        let queue_family_index = queue_families.iter().position(|qf| {
            qf.queue_flags.contains(vk::QueueFlags::GRAPHICS)
        }).ok_or("No graphics queue family")? as u32;

        // Create device with required extensions
        let dev_ext_names = [
            cstr("VK_KHR_external_memory"),
            cstr("VK_KHR_external_memory_fd"),
            cstr("VK_ANDROID_external_memory_android_hardware_buffer"),
            cstr("VK_KHR_external_fence"),
            cstr("VK_KHR_external_fence_fd"),
        ];
        let dev_ext_ptrs: Vec<*const std::ffi::c_char> = dev_ext_names.iter().map(|s| s.as_ptr()).collect();

        let priority = 1.0f32;
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(std::slice::from_ref(&priority));

        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_info))
            .enabled_extension_names(&dev_ext_ptrs);

        let device = unsafe { instance.create_device(physical_device, &device_info, None) }
            .map_err(|e| format!("vkCreateDevice: {e}"))?;

        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        let external_memory_fd_fn = khr::external_memory_fd::Device::new(&instance, &device);
        let ahb_fn = ash::android::external_memory_android_hardware_buffer::Device::new(&instance, &device);
        let external_fence_fd_fn = khr::external_fence_fd::Device::new(&instance, &device);

        // Persistent command pool + buffer (reused every frame).
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let cmd_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .map_err(|e| format!("create_command_pool: {e}"))?;

        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(cmd_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd_buf = unsafe { device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| format!("allocate_command_buffers: {e}"))?[0];

        tracing::info!("[vk-renderer] Vulkan renderer initialized");

        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            _queue_family_index: queue_family_index,
            external_memory_fd_fn,
            ahb_fn,
            external_fence_fd_fn,
            _cmd_pool: cmd_pool,
            cmd_buf,
            staging_cache: std::cell::RefCell::new(None),
            dmabuf_cache: std::cell::RefCell::new(HashMap::new()),
            shm_staging: std::cell::RefCell::new(None),
        })
    }
}
