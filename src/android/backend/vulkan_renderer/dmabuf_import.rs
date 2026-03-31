//! Dmabuf import and caching logic.

use ash::vk;
use std::os::unix::io::RawFd;

use super::{ImportedDmabuf, VulkanRenderer};

impl VulkanRenderer {
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

    /// Clear the dmabuf cache.
    pub fn clear_dmabuf_cache(&self) {
        let old = self.dmabuf_cache.borrow_mut().drain().collect::<Vec<_>>();
        for (fd, imported) in &old {
            tracing::info!("[vk-renderer] Clearing cached dmabuf fd={} ({}x{})", fd, imported.width, imported.height);
            self.destroy_imported(imported);
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
