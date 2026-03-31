//! Composition/blit operations: dmabuf and shm to AHB.

use ash::vk;
use std::os::unix::io::RawFd;

use super::{AhbTarget, ImportedDmabuf, ShmStaging, StagingImage, VulkanRenderer};

impl VulkanRenderer {
    /// Blit an imported dmabuf onto an AHB-backed VkImage for ASurfaceTransaction.
    /// Returns a sync fd that signals when the GPU blit completes, or -1 on failure.
    /// The caller passes this fd to ASurfaceTransaction_setBuffer as acquire_fence_fd.
    pub fn blit_dmabuf_to_ahb(
        &self,
        dmabuf: &ImportedDmabuf,
        target: &AhbTarget,
    ) -> Result<RawFd, String> {
        let cmd = self.cmd_buf;

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

        // Two-step: dmabuf buffer -> LINEAR staging -> AHB image (BGRA->RGBA blit)
        let staging_img = self.get_or_create_staging(dmabuf.width, dmabuf.height,
            Self::fourcc_to_vk_format(0x34325258))?;

        let staging_to_dst = vk::ImageMemoryBarrier::default()
            .image(staging_img)
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
            self.device.cmd_copy_buffer_to_image(cmd, dmabuf.buffer, staging_img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy_region]);
        }

        let staging_to_src = vk::ImageMemoryBarrier::default()
            .image(staging_img)
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
            self.device.cmd_blit_image(cmd, staging_img,
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

        // Create exportable fence (SYNC_FD) so SurfaceFlinger waits for the GPU
        // instead of us blocking on vkQueueWaitIdle.
        let mut export_info = vk::ExportFenceCreateInfo::default()
            .handle_types(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        let fence_info = vk::FenceCreateInfo::default()
            .push_next(&mut export_info);
        let fence = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| format!("create_fence: {e}"))?;

        let submit_info = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        unsafe {
            self.device.queue_submit(self.queue, &[submit_info], fence)
                .map_err(|e| format!("queue_submit: {e}"))?;
        }

        // Export fence as sync fd for ASurfaceTransaction
        let fd_info = vk::FenceGetFdInfoKHR::default()
            .fence(fence)
            .handle_type(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        let sync_fd = unsafe { self.external_fence_fd_fn.get_fence_fd(&fd_info) }
            .unwrap_or_else(|e| {
                tracing::warn!("[vk-renderer] get_fence_fd failed: {e}, falling back to wait");
                unsafe { let _ = self.device.wait_for_fences(&[fence], true, u64::MAX); }
                -1
            });

        // SYNC_FD export transfers ownership — Vulkan fence is now unsignaled/consumed.
        // Destroy it immediately; the sync fd is what SurfaceFlinger will wait on.
        unsafe { self.device.destroy_fence(fence, None) };

        Ok(sync_fd)
    }

    // -- SHM -> AHB blit (CPU pixels -> Vulkan -> ASurfaceTransaction) ------

    /// Get or create a host-visible staging buffer for shm uploads.
    fn get_or_create_shm_staging(&self, needed: u64) -> Result<(), String> {
        {
            let cache = self.shm_staging.borrow();
            if let Some(ref s) = *cache {
                if s.size >= needed { return Ok(()); }
            }
        }
        // Destroy old
        {
            let mut cache = self.shm_staging.borrow_mut();
            if let Some(old) = cache.take() {
                let _ = unsafe { self.device.device_wait_idle() };
                unsafe {
                    self.device.unmap_memory(old.memory);
                    self.device.destroy_buffer(old.buffer, None);
                    self.device.free_memory(old.memory, None);
                }
            }
        }
        let buf_info = vk::BufferCreateInfo::default()
            .size(needed)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { self.device.create_buffer(&buf_info, None) }
            .map_err(|e| format!("create shm staging buffer: {e}"))?;
        let reqs = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let mem_type = self.find_memory_type(reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT)?;
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(mem_type);
        let memory = unsafe { self.device.allocate_memory(&alloc, None) }
            .map_err(|e| format!("alloc shm staging: {e}"))?;
        unsafe { self.device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|e| format!("bind shm staging: {e}"))?;
        let mapped = unsafe {
            self.device.map_memory(memory, 0, reqs.size, vk::MemoryMapFlags::empty())
        }.map_err(|e| format!("map shm staging: {e}"))? as *mut u8;
        tracing::info!("[vk-renderer] Created shm staging buffer {} bytes", reqs.size);
        *self.shm_staging.borrow_mut() = Some(ShmStaging { buffer, memory, mapped, size: reqs.size });
        Ok(())
    }

    /// Blit shm pixel data onto an AHB-backed VkImage for ASurfaceTransaction.
    /// `data` points to the raw pixel buffer, `stride` is bytes per row.
    /// Returns a sync fd for ASurfaceTransaction acquire fence.
    pub fn blit_shm_to_ahb(
        &self,
        data: *const u8,
        width: u32,
        height: u32,
        stride: u32,
        _format: vk::Format,
        target: &AhbTarget,
    ) -> Result<RawFd, String> {
        let buf_size = (stride as u64) * (height as u64);
        self.get_or_create_shm_staging(buf_size)?;

        // Copy pixel data into staging buffer
        {
            let cache = self.shm_staging.borrow();
            let staging = cache.as_ref().ok_or("no staging")?;
            unsafe {
                std::ptr::copy_nonoverlapping(data, staging.mapped, buf_size as usize);
            }
        }

        let cmd = self.cmd_buf;
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { self.device.begin_command_buffer(cmd, &begin_info) }
            .map_err(|e| format!("begin_cmd_buf(shm): {e}"))?;

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0, level_count: 1,
            base_array_layer: 0, layer_count: 1,
        };
        let color_layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0, base_array_layer: 0, layer_count: 1,
        };

        // Transition AHB to TRANSFER_DST
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

        // Copy staging buffer -> staging image -> AHB (same two-step as dmabuf path
        // for BGRA->RGBA conversion via vkCmdBlitImage)
        let stride_pixels = stride / 4;
        let staging_img = self.get_or_create_staging(width, height,
            Self::fourcc_to_vk_format(0x34325258))?;

        let staging_to_dst = vk::ImageMemoryBarrier::default()
            .image(staging_img)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(color_range);
        unsafe {
            self.device.cmd_pipeline_barrier(cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[staging_to_dst]);
        }

        let staging_buf = self.shm_staging.borrow();
        let staging_vk_buf = staging_buf.as_ref().ok_or("no staging")?.buffer;
        let copy_region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: stride_pixels,
            buffer_image_height: height,
            image_subresource: color_layers,
            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
            image_extent: vk::Extent3D { width, height, depth: 1 },
        };
        unsafe {
            self.device.cmd_copy_buffer_to_image(cmd, staging_vk_buf, staging_img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy_region]);
        }
        drop(staging_buf);

        let staging_to_src = vk::ImageMemoryBarrier::default()
            .image(staging_img)
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

        let blit_w = width.min(target.width) as i32;
        let blit_h = height.min(target.height) as i32;
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
            self.device.cmd_blit_image(cmd, staging_img,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                target.vk_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit_region], vk::Filter::NEAREST);
        }

        // Transition AHB to GENERAL
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
            .map_err(|e| format!("end_cmd_buf(shm): {e}"))?;

        // Submit with exportable fence
        let mut export_info = vk::ExportFenceCreateInfo::default()
            .handle_types(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        let fence_info = vk::FenceCreateInfo::default().push_next(&mut export_info);
        let fence = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| format!("create_fence(shm): {e}"))?;

        let submit = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        unsafe { self.device.queue_submit(self.queue, &[submit], fence) }
            .map_err(|e| format!("queue_submit(shm): {e}"))?;

        let fd_info = vk::FenceGetFdInfoKHR::default()
            .fence(fence)
            .handle_type(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        let sync_fd = unsafe { self.external_fence_fd_fn.get_fence_fd(&fd_info) }
            .map_err(|e| format!("get_fence_fd(shm): {e}"))?;

        unsafe { self.device.destroy_fence(fence, None) };

        Ok(sync_fd)
    }

    // -- Helpers -------------------------------------------------------------

    /// Get or create a cached LINEAR staging image. Recreates only when size changes.
    fn get_or_create_staging(&self, width: u32, height: u32, format: vk::Format)
        -> Result<vk::Image, String>
    {
        {
            let cache = self.staging_cache.borrow();
            if let Some(ref s) = *cache {
                if s.width == width && s.height == height {
                    return Ok(s.image);
                }
            }
        }
        // Destroy old staging if size changed
        {
            let mut cache = self.staging_cache.borrow_mut();
            if let Some(old) = cache.take() {
                let _ = unsafe { self.device.device_wait_idle() };
                unsafe {
                    self.device.destroy_image(old.image, None);
                    self.device.free_memory(old.memory, None);
                }
            }
        }
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
        tracing::info!("[vk-renderer] Created staging image {}x{}", width, height);
        *self.staging_cache.borrow_mut() = Some(StagingImage { image, memory, width, height });
        Ok(image)
    }

    /// Map DRM fourcc to VkFormat.
    pub fn fourcc_to_vk_format(fourcc: u32) -> vk::Format {
        match fourcc {
            0x34325258 | 0x34325241 => vk::Format::B8G8R8A8_UNORM,
            0x34324258 | 0x34324241 => vk::Format::R8G8B8A8_UNORM,
            _ => vk::Format::B8G8R8A8_UNORM,
        }
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
}
