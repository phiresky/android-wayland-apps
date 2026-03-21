//! Vulkan renderer for zero-copy dmabuf compositing on Android.
//!
//! Uses the proprietary Qualcomm Vulkan driver to import client dmabufs
//! (from Turnip/KGSL) and blit them onto AHardwareBuffer targets for
//! presentation via ASurfaceTransaction.

use ash::vk;
use ash::khr;
use std::collections::HashMap;
use std::ffi::{c_char, CString};
use std::os::unix::io::RawFd;

use super::surface_transaction::{
    HardwareBuffer, AHB_FORMAT_R8G8B8A8_UNORM,
    AHB_USAGE_GPU_FRAMEBUFFER, AHB_USAGE_GPU_SAMPLED_IMAGE, AHB_USAGE_COMPOSER_OVERLAY,
};

/// Vulkan renderer state for compositing client surfaces onto Android windows.
pub struct VulkanRenderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    external_memory_fd_fn: khr::external_memory_fd::Device,
    ahb_fn: ash::android::external_memory_android_hardware_buffer::Device,
    /// Cache imported dmabufs by fd to avoid re-importing every frame.
    /// Clients reuse a small pool of ~3 buffers.
    dmabuf_cache: std::cell::RefCell<HashMap<RawFd, ImportedDmabuf>>,
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
        ];
        let dev_ext_ptrs: Vec<*const c_char> = dev_ext_names.iter().map(|s| s.as_ptr()).collect();

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

        tracing::info!("[vk-renderer] Vulkan renderer initialized");

        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            queue_family_index,
            external_memory_fd_fn,
            ahb_fn,
            dmabuf_cache: std::cell::RefCell::new(HashMap::new()),
        })
    }

    // ── Dmabuf import ──────────────────────────────────────────────────────

    /// Get or import a dmabuf. Caches by fd for reuse across frames.
    pub fn get_or_import_dmabuf(
        &self,
        fd: RawFd,
        width: u32,
        height: u32,
        stride: u32,
        format: vk::Format,
    ) -> Result<std::cell::Ref<'_, ImportedDmabuf>, String> {
        {
            let cache = self.dmabuf_cache.borrow();
            if let Some(cached) = cache.get(&fd) {
                if cached.width == width && cached.height == height {
                    drop(cache);
                    return Ok(std::cell::Ref::map(self.dmabuf_cache.borrow(), |c| {
                        c.get(&fd).unwrap_or_else(|| unreachable!())
                    }));
                }
                tracing::info!("[vk-renderer] Evicting stale cache fd={} (was {}x{}, now {}x{})",
                    fd, cached.width, cached.height, width, height);
            }
        }
        let imported = self.import_dmabuf(fd, width, height, stride, format)?;
        tracing::info!("[vk-renderer] Cached dmabuf fd={} ({}x{}, cache size={})",
            fd, width, height, self.dmabuf_cache.borrow().len() + 1);
        self.dmabuf_cache.borrow_mut().insert(fd, imported);
        Ok(std::cell::Ref::map(self.dmabuf_cache.borrow(), |c| {
            c.get(&fd).unwrap_or_else(|| unreachable!())
        }))
    }

    /// Import a dmabuf fd as a VkImage + VkBuffer (zero-copy via KGSL).
    pub fn import_dmabuf(
        &self,
        fd: RawFd,
        width: u32,
        height: u32,
        stride: u32,
        format: vk::Format,
    ) -> Result<ImportedDmabuf, String> {
        let fd_dup = unsafe { libc::dup(fd) };
        if fd_dup < 0 { return Err("dup failed".into()); }

        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        unsafe {
            self.external_memory_fd_fn.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                fd_dup,
                &mut fd_props,
            )
        }.map_err(|e| format!("get_memory_fd_properties: {e}"))?;

        if fd_props.memory_type_bits == 0 {
            unsafe { libc::close(fd_dup); }
            return Err("No compatible memory types for dmabuf".into());
        }
        let mem_type_index = fd_props.memory_type_bits.trailing_zeros();

        let mut external_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external_info)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("vkCreateImage: {e}"))?;

        let mem_reqs = unsafe { self.device.get_image_memory_requirements(image) };

        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd_dup);

        let alloc_info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_info)
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type_index);

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| format!("vkAllocateMemory(import): {e}"))?;

        unsafe { self.device.bind_image_memory(image, memory, 0) }
            .map_err(|e| format!("vkBindImageMemory: {e}"))?;

        let buf_size = (stride as u64) * (height as u64);
        let buffer_info = vk::BufferCreateInfo::default()
            .size(buf_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe { self.device.create_buffer(&buffer_info, None) }
            .map_err(|e| format!("vkCreateBuffer: {e}"))?;

        unsafe { self.device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|e| format!("vkBindBufferMemory: {e}"))?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0, level_count: 1,
                base_array_layer: 0, layer_count: 1,
            });

        let view = unsafe { self.device.create_image_view(&view_info, None) }
            .map_err(|e| format!("vkCreateImageView: {e}"))?;

        let stride_pixels = stride / 4;

        tracing::debug!("[vk-renderer] Imported dmabuf {}x{} stride={}", width, height, stride);

        Ok(ImportedDmabuf { image, buffer, memory, view, width, height, stride_pixels })
    }

    // ── AHardwareBuffer target ─────────────────────────────────────────────

    /// Allocate an AHardwareBuffer and import it into Vulkan as a TRANSFER_DST image.
    pub fn create_ahb_target(&self, width: u32, height: u32) -> Result<AhbTarget, String> {
        let ahb = HardwareBuffer::allocate(
            width, height,
            AHB_FORMAT_R8G8B8A8_UNORM,
            AHB_USAGE_GPU_FRAMEBUFFER | AHB_USAGE_GPU_SAMPLED_IMAGE | AHB_USAGE_COMPOSER_OVERLAY,
        ).ok_or("AHardwareBuffer_allocate failed")?;

        let mut ahb_props = vk::AndroidHardwareBufferPropertiesANDROID::default();
        unsafe {
            self.ahb_fn.get_android_hardware_buffer_properties(ahb.as_ptr().cast(), &mut ahb_props)
        }.map_err(|e| format!("get_android_hardware_buffer_properties: {e}"))?;

        let mem_type_index = ahb_props.memory_type_bits.trailing_zeros();
        tracing::info!("[vk-ahb] AHB props: alloc_size={}, mem_type_bits=0x{:x}",
            ahb_props.allocation_size, ahb_props.memory_type_bits);

        let mut external_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);

        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external_info)
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let vk_image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("create_image(ahb): {e}"))?;

        let mut import_ahb = vk::ImportAndroidHardwareBufferInfoANDROID::default()
            .buffer(ahb.as_ptr().cast());

        let alloc_info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_ahb)
            .allocation_size(ahb_props.allocation_size)
            .memory_type_index(mem_type_index);

        let vk_memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| format!("allocate_memory(ahb): {e}"))?;

        unsafe { self.device.bind_image_memory(vk_image, vk_memory, 0) }
            .map_err(|e| format!("bind_image_memory(ahb): {e}"))?;

        tracing::info!("[vk-ahb] Created AHB target {}x{}", width, height);

        Ok(AhbTarget { ahb, vk_image, vk_memory, width, height })
    }

    /// Blit an imported dmabuf onto an AHB-backed VkImage for ASurfaceTransaction.
    pub fn blit_dmabuf_to_ahb(
        &self,
        dmabuf: &ImportedDmabuf,
        target: &AhbTarget,
    ) -> Result<(), String> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let pool = unsafe { self.device.create_command_pool(&pool_info, None) }
            .map_err(|e| format!("create_cmd_pool: {e}"))?;

        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| format!("alloc_cmd_buf: {e}"))?[0];

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { self.device.begin_command_buffer(cmd, &begin_info) }
            .map_err(|e| format!("begin_cmd_buf: {e}"))?;

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0, level_count: 1,
            base_array_layer: 0, layer_count: 1,
        };
        let color_layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0, base_array_layer: 0, layer_count: 1,
        };

        // Transition AHB image to TRANSFER_DST
        let dst_barrier = vk::ImageMemoryBarrier::default()
            .image(target.vk_image)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(color_range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[dst_barrier]);
        }

        // Clear to black (client may be smaller than AHB)
        let clear_color = vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] };
        unsafe {
            self.device.cmd_clear_color_image(cmd,
                target.vk_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear_color, &[color_range]);
        }

        // Two-step: dmabuf buffer → LINEAR staging → AHB image (BGRA→RGBA blit)
        let staging = self.get_or_create_staging(dmabuf.width, dmabuf.height,
            Self::fourcc_to_vk_format(0x34325258))?;

        let staging_to_dst = vk::ImageMemoryBarrier::default()
            .image(staging.0)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(color_range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[staging_to_dst]);
        }

        let copy_region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: dmabuf.stride_pixels,
            buffer_image_height: dmabuf.height,
            image_subresource: color_layers,
            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
            image_extent: vk::Extent3D { width: dmabuf.width, height: dmabuf.height, depth: 1 },
        };
        unsafe {
            self.device.cmd_copy_buffer_to_image(cmd, dmabuf.buffer, staging.0,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy_region]);
        }

        let staging_to_src = vk::ImageMemoryBarrier::default()
            .image(staging.0)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .subresource_range(color_range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[staging_to_src]);
        }

        let blit_w = dmabuf.width.min(target.width) as i32;
        let blit_h = dmabuf.height.min(target.height) as i32;
        let blit_region = vk::ImageBlit {
            src_subresource: color_layers,
            src_offsets: [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: blit_w, y: blit_h, z: 1 },
            ],
            dst_subresource: color_layers,
            dst_offsets: [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: blit_w, y: blit_h, z: 1 },
            ],
        };
        unsafe {
            self.device.cmd_blit_image(cmd, staging.0,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                target.vk_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit_region], vk::Filter::NEAREST);
        }

        // Transition AHB image to GENERAL (ready for SurfaceFlinger)
        let final_barrier = vk::ImageMemoryBarrier::default()
            .image(target.vk_image)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(color_range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(), &[], &[], &[final_barrier]);
        }

        unsafe { self.device.end_command_buffer(cmd) }
            .map_err(|e| format!("end_cmd_buf: {e}"))?;

        let submit_info = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        unsafe {
            self.device.queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .map_err(|e| format!("queue_submit: {e}"))?;
            self.device.queue_wait_idle(self.queue)
                .map_err(|e| format!("queue_wait_idle: {e}"))?;
        }

        unsafe {
            self.device.free_command_buffers(pool, &[cmd]);
            self.device.destroy_command_pool(pool, None);
            self.device.destroy_image(staging.0, None);
            self.device.free_memory(staging.1, None);
        }

        Ok(())
    }

    /// Destroy an AHB target's Vulkan resources. The AHB itself is released by Drop.
    pub fn destroy_ahb_target(&self, target: &AhbTarget) {
        let _ = unsafe { self.device.device_wait_idle() };
        unsafe {
            self.device.destroy_image(target.vk_image, None);
            self.device.free_memory(target.vk_memory, None);
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn get_or_create_staging(&self, width: u32, height: u32, format: vk::Format)
        -> Result<(vk::Image, vk::DeviceMemory), String>
    {
        let staging_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let image = unsafe { self.device.create_image(&staging_info, None) }
            .map_err(|e| format!("create staging image: {e}"))?;
        let reqs = unsafe { self.device.get_image_memory_requirements(image) };
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(self.find_memory_type(reqs.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL)?);
        let memory = unsafe { self.device.allocate_memory(&alloc, None) }
            .map_err(|e| format!("alloc staging memory: {e}"))?;
        unsafe { self.device.bind_image_memory(image, memory, 0) }
            .map_err(|e| format!("bind staging memory: {e}"))?;
        Ok((image, memory))
    }

    fn find_memory_type(&self, type_filter: u32, properties: vk::MemoryPropertyFlags) -> Result<u32, String> {
        let mem_props = unsafe {
            self.instance.get_physical_device_memory_properties(self.physical_device)
        };
        for i in 0..mem_props.memory_type_count {
            if (type_filter & (1 << i)) != 0
                && mem_props.memory_types[i as usize].property_flags.contains(properties)
            {
                return Ok(i);
            }
        }
        Err("No suitable memory type".into())
    }

    /// Clear the dmabuf cache.
    pub fn clear_dmabuf_cache(&self) {
        let old = self.dmabuf_cache.borrow_mut().drain().collect::<Vec<_>>();
        for (fd, imported) in &old {
            tracing::info!("[vk-renderer] Clearing cached dmabuf fd={} ({}x{})", fd, imported.width, imported.height);
            self.destroy_imported(imported);
        }
    }

    /// Map DRM fourcc to VkFormat.
    pub fn fourcc_to_vk_format(fourcc: u32) -> vk::Format {
        match fourcc {
            0x34325258 | 0x34325241 => vk::Format::B8G8R8A8_UNORM,
            0x34324258 | 0x34324241 => vk::Format::R8G8B8A8_UNORM,
            _ => vk::Format::B8G8R8A8_UNORM,
        }
    }

    /// Destroy an imported dmabuf's Vulkan resources.
    pub fn destroy_imported(&self, imported: &ImportedDmabuf) {
        unsafe {
            self.device.destroy_image_view(imported.view, None);
            self.device.destroy_image(imported.image, None);
            self.device.destroy_buffer(imported.buffer, None);
            self.device.free_memory(imported.memory, None);
        }
    }
}
