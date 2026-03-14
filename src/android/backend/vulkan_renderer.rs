//! Vulkan renderer for zero-copy dmabuf compositing on Android.
//!
//! Uses the proprietary Qualcomm Vulkan driver to import client dmabufs
//! (from Turnip/KGSL) and composite them onto Android surfaces. Both drivers
//! use KGSL, so dmabuf import is zero-copy.

use ash::vk;
use ash::khr;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::os::unix::io::RawFd;

/// Vulkan renderer state for compositing client surfaces onto Android windows.
pub struct VulkanRenderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    swapchain_fn: khr::swapchain::Device,
    external_memory_fd_fn: khr::external_memory_fd::Device,
    /// Cache imported dmabufs by fd to avoid re-importing every frame.
    /// Clients reuse a small pool of ~3 swapchain buffers.
    dmabuf_cache: std::cell::RefCell<HashMap<RawFd, ImportedDmabuf>>,
}

/// Per-window swapchain state.
pub struct VulkanWindowSurface {
    pub surface: vk::SurfaceKHR,
    pub swapchain: vk::SwapchainKHR,
    pub images: Vec<vk::Image>,
    pub image_views: Vec<vk::ImageView>,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
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

        // Create instance with android_surface
        let app_name = cstr("wayland-compositor");
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(1)
            .api_version(vk::make_api_version(0, 1, 1, 0));

        let inst_ext_names = [
            cstr("VK_KHR_surface"),
            cstr("VK_KHR_android_surface"),
        ];
        let inst_ext_ptrs: Vec<*const c_char> = inst_ext_names.iter().map(|s| s.as_ptr()).collect();

        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&inst_ext_ptrs);

        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|e| format!("vkCreateInstance: {e}"))?;

        // Select physical device
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate_physical_devices: {e}"))?;
        let physical_device = physical_devices.into_iter().next()
            .ok_or("No Vulkan physical device")?;

        let props = unsafe { instance.get_physical_device_properties(physical_device) };
        let name = unsafe { std::ffi::CStr::from_ptr(props.device_name.as_ptr()) };
        log::info!("[vk-renderer] GPU: {:?}", name);

        // Find graphics queue family
        let queue_families = unsafe {
            instance.get_physical_device_queue_family_properties(physical_device)
        };
        let queue_family_index = queue_families.iter().position(|qf| {
            qf.queue_flags.contains(vk::QueueFlags::GRAPHICS)
        }).ok_or("No graphics queue family")? as u32;

        // Create device with required extensions
        let dev_ext_names = [
            cstr("VK_KHR_swapchain"),
            cstr("VK_KHR_external_memory"),
            cstr("VK_KHR_external_memory_fd"),
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

        let swapchain_fn = khr::swapchain::Device::new(&instance, &device);
        let external_memory_fd_fn = khr::external_memory_fd::Device::new(&instance, &device);

        log::info!("[vk-renderer] Vulkan renderer initialized");

        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            queue_family_index,
            swapchain_fn,
            external_memory_fd_fn,
            dmabuf_cache: std::cell::RefCell::new(HashMap::new()),
        })
    }

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
            if cache.contains_key(&fd) {
                drop(cache);
                return Ok(std::cell::Ref::map(self.dmabuf_cache.borrow(), |c| {
                    c.get(&fd).unwrap_or_else(|| unreachable!())
                }));
            }
        }
        // Not cached — import and store
        let imported = self.import_dmabuf(fd, width, height, stride, format)?;
        log::info!("[vk-renderer] Cached dmabuf fd={} ({}x{}, cache size={})",
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

        // Query memory properties for this fd
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

        // Create image with external memory flag
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

        // Import the dmabuf fd as device memory
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd_dup); // Vulkan takes ownership

        let alloc_info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_info)
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type_index);

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| format!("vkAllocateMemory(import): {e}"))?;

        unsafe { self.device.bind_image_memory(image, memory, 0) }
            .map_err(|e| format!("vkBindImageMemory: {e}"))?;

        // Also create a VkBuffer bound to the same memory for stride-aware copies
        let buf_size = (stride as u64) * (height as u64);
        let buffer_info = vk::BufferCreateInfo::default()
            .size(buf_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe { self.device.create_buffer(&buffer_info, None) }
            .map_err(|e| format!("vkCreateBuffer: {e}"))?;

        unsafe { self.device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|e| format!("vkBindBufferMemory: {e}"))?;

        // Create image view for sampling
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });

        let view = unsafe { self.device.create_image_view(&view_info, None) }
            .map_err(|e| format!("vkCreateImageView: {e}"))?;

        let stride_pixels = stride / 4; // 4 bytes per pixel for RGBA/BGRA
        log::debug!("[vk-renderer] Imported dmabuf {}x{} stride={}", width, height, stride);

        Ok(ImportedDmabuf { image, buffer, memory, view, width, height, stride_pixels })
    }

    /// Create a swapchain for an Android native window.
    pub fn create_window_surface(
        &self,
        native_window: *mut c_void,
    ) -> Result<VulkanWindowSurface, String> {
        let android_surface_fn = khr::android_surface::Instance::new(&self._entry, &self.instance);
        let surface_info = vk::AndroidSurfaceCreateInfoKHR::default()
            .window(native_window);

        let surface = unsafe { android_surface_fn.create_android_surface(&surface_info, None) }
            .map_err(|e| format!("create_android_surface: {e}"))?;

        let surface_fn = khr::surface::Instance::new(&self._entry, &self.instance);
        let caps = unsafe {
            surface_fn.get_physical_device_surface_capabilities(self.physical_device, surface)
        }.map_err(|e| format!("get_surface_capabilities: {e}"))?;

        let formats = unsafe {
            surface_fn.get_physical_device_surface_formats(self.physical_device, surface)
        }.map_err(|e| format!("get_surface_formats: {e}"))?;

        let format = formats.iter()
            .find(|f| f.format == vk::Format::R8G8B8A8_UNORM || f.format == vk::Format::B8G8R8A8_UNORM)
            .or(formats.first())
            .ok_or("No surface formats")?;

        log::info!("[vk-renderer] Surface format: {:?}, extent: {}x{}",
            format.format.as_raw(), caps.current_extent.width, caps.current_extent.height);

        let image_count = (caps.min_image_count + 1).min(
            if caps.max_image_count == 0 { u32::MAX } else { caps.max_image_count }
        );

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D { width: 1920, height: 1080 }
        };

        let swapchain_info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true);

        let swapchain = unsafe { self.swapchain_fn.create_swapchain(&swapchain_info, None) }
            .map_err(|e| format!("create_swapchain: {e}"))?;

        let images = unsafe { self.swapchain_fn.get_swapchain_images(swapchain) }
            .map_err(|e| format!("get_swapchain_images: {e}"))?;

        let image_views: Vec<vk::ImageView> = images.iter().map(|&img| {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(img)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format.format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0, level_count: 1,
                    base_array_layer: 0, layer_count: 1,
                });
            unsafe { self.device.create_image_view(&view_info, None) }
        }).collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("create_image_view: {e}"))?;

        log::info!("[vk-renderer] Swapchain created: {}x{}, {} images",
            extent.width, extent.height, images.len());

        Ok(VulkanWindowSurface {
            surface, swapchain, images, image_views,
            format: format.format, extent,
        })
    }

    /// Present a solid color to a window surface (test that presentation works).
    pub fn present_clear_color(
        &self,
        window: &VulkanWindowSurface,
        r: f32, g: f32, b: f32,
    ) -> Result<(), String> {
        let (image_index, _) = unsafe {
            self.swapchain_fn.acquire_next_image(
                window.swapchain, u64::MAX,
                vk::Semaphore::null(), vk::Fence::null(),
            )
        }.map_err(|e| format!("acquire_next_image: {e}"))?;

        // Create command pool + buffer (TODO: cache)
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let pool = unsafe { self.device.create_command_pool(&pool_info, None) }
            .map_err(|e| format!("create_command_pool: {e}"))?;

        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| format!("allocate_command_buffers: {e}"))?[0];

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { self.device.begin_command_buffer(cmd, &begin_info) }
            .map_err(|e| format!("begin_command_buffer: {e}"))?;

        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0, level_count: 1,
            base_array_layer: 0, layer_count: 1,
        };

        // Transition to TRANSFER_DST
        let barrier = vk::ImageMemoryBarrier::default()
            .image(window.images[image_index as usize])
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[barrier]);
        }

        // Clear
        let clear_color = vk::ClearColorValue { float32: [r, g, b, 1.0] };
        unsafe {
            self.device.cmd_clear_color_image(cmd,
                window.images[image_index as usize],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear_color, &[range]);
        }

        // Transition to PRESENT
        let barrier2 = vk::ImageMemoryBarrier::default()
            .image(window.images[image_index as usize])
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(), &[], &[], &[barrier2]);
        }

        unsafe { self.device.end_command_buffer(cmd) }
            .map_err(|e| format!("end_command_buffer: {e}"))?;

        // Submit + wait + present
        let submit_info = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        unsafe {
            self.device.queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .map_err(|e| format!("queue_submit: {e}"))?;
            self.device.queue_wait_idle(self.queue)
                .map_err(|e| format!("queue_wait_idle: {e}"))?;
        }

        let present_info = vk::PresentInfoKHR::default()
            .swapchains(std::slice::from_ref(&window.swapchain))
            .image_indices(std::slice::from_ref(&image_index));
        unsafe { self.swapchain_fn.queue_present(self.queue, &present_info) }
            .map_err(|e| format!("queue_present: {e}"))?;

        // Cleanup (TODO: don't recreate every frame)
        unsafe {
            self.device.free_command_buffers(pool, &[cmd]);
            self.device.destroy_command_pool(pool, None);
        }

        Ok(())
    }

    /// Copy an imported dmabuf onto a swapchain image and present.
    /// Uses VkBuffer + vkCmdCopyBufferToImage for explicit stride control.
    pub fn blit_dmabuf_to_swapchain(
        &self,
        dmabuf: &ImportedDmabuf,
        window: &VulkanWindowSurface,
    ) -> Result<(), String> {
        let (image_index, _) = unsafe {
            self.swapchain_fn.acquire_next_image(
                window.swapchain, u64::MAX,
                vk::Semaphore::null(), vk::Fence::null(),
            )
        }.map_err(|e| format!("acquire_next_image: {e}"))?;

        // Command pool + buffer (TODO: cache)
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

        // Transition destination (swapchain image) to TRANSFER_DST
        let dst_barrier = vk::ImageMemoryBarrier::default()
            .image(window.images[image_index as usize])
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(color_range);

        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[dst_barrier]);
        }

        // Clear swapchain image to black first (client may be smaller than swapchain)
        let clear_color = vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] };
        unsafe {
            self.device.cmd_clear_color_image(cmd,
                window.images[image_index as usize],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear_color, &[color_range]);
        }

        // Copy from dmabuf (as VkBuffer) to swapchain image with explicit stride.
        // Using the VkBuffer view of the imported memory avoids tiling interpretation.
        let copy_region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: dmabuf.stride_pixels,   // explicit row pitch in pixels
            buffer_image_height: dmabuf.height,
            image_subresource: color_layers,
            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
            image_extent: vk::Extent3D {
                width: dmabuf.width.min(window.extent.width),
                height: dmabuf.height.min(window.extent.height),
                depth: 1,
            },
        };
        unsafe {
            self.device.cmd_copy_buffer_to_image(cmd,
                dmabuf.buffer,
                window.images[image_index as usize],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy_region]);
        }

        // Transition swapchain image to PRESENT
        let present_barrier = vk::ImageMemoryBarrier::default()
            .image(window.images[image_index as usize])
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(color_range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(), &[], &[], &[present_barrier]);
        }

        unsafe { self.device.end_command_buffer(cmd) }
            .map_err(|e| format!("end_cmd_buf: {e}"))?;

        // Submit + wait + present
        let submit_info = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        unsafe {
            self.device.queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .map_err(|e| format!("queue_submit: {e}"))?;
            self.device.queue_wait_idle(self.queue)
                .map_err(|e| format!("queue_wait_idle: {e}"))?;
        }

        let present_info = vk::PresentInfoKHR::default()
            .swapchains(std::slice::from_ref(&window.swapchain))
            .image_indices(std::slice::from_ref(&image_index));
        unsafe { self.swapchain_fn.queue_present(self.queue, &present_info) }
            .map_err(|e| format!("queue_present: {e}"))?;

        unsafe {
            self.device.free_command_buffers(pool, &[cmd]);
            self.device.destroy_command_pool(pool, None);
        }

        Ok(())
    }

    /// Map DRM fourcc to VkFormat.
    pub fn fourcc_to_vk_format(fourcc: u32) -> vk::Format {
        match fourcc {
            // DRM_FORMAT_XRGB8888 = "XR24"
            0x34325258 => vk::Format::B8G8R8A8_UNORM,
            // DRM_FORMAT_ARGB8888 = "AR24"
            0x34325241 => vk::Format::B8G8R8A8_UNORM,
            // DRM_FORMAT_XBGR8888 = "XB24"
            0x34324258 => vk::Format::R8G8B8A8_UNORM,
            // DRM_FORMAT_ABGR8888 = "AB24"
            0x34324241 => vk::Format::R8G8B8A8_UNORM,
            _ => vk::Format::B8G8R8A8_UNORM, // fallback
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

    pub fn device(&self) -> &ash::Device { &self.device }
    pub fn instance(&self) -> &ash::Instance { &self.instance }
}
