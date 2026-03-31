//! AHardwareBuffer target allocation and management.

use ash::vk;

use crate::android::backend::surface_transaction::{
    HardwareBuffer, AHB_FORMAT_R8G8B8A8_UNORM,
    AHB_USAGE_GPU_FRAMEBUFFER, AHB_USAGE_GPU_SAMPLED_IMAGE, AHB_USAGE_COMPOSER_OVERLAY,
};
use super::{AhbTarget, VulkanRenderer};

impl VulkanRenderer {
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

    /// Destroy an AHB target's Vulkan resources. The AHB itself is released by Drop.
    pub fn destroy_ahb_target(&self, target: &AhbTarget) {
        let _ = unsafe { self.device.device_wait_idle() };
        unsafe {
            self.device.destroy_image(target.vk_image, None);
            self.device.free_memory(target.vk_memory, None);
        }
    }
}
