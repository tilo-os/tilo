//! DMA-BUF → wgpu texture import via Vulkan external memory.
//!
//! WebKit exports a GPU buffer as a dmabuf fd (see [`super::egl`]); this
//! wraps it as a sampleable `wgpu::Texture` with **no copy** — the texture
//! aliases the same GPU memory WebKit rendered into. Works on a **linear
//! single-plane** buffer (`AMD_DEBUG=nodcc,notiling`), needing only
//! `VK_EXT_external_memory_dma_buf` + `VK_KHR_external_memory_fd`, both
//! already enabled by wgpu-hal. Tiled/compressed buffers would additionally
//! need `VK_EXT_image_drm_format_modifier`, which wgpu-hal does not enable.

use std::os::fd::{AsRawFd, IntoRawFd};

use ash::vk;
use vello::wgpu;
use vello::wgpu::hal::vulkan;

use super::egl::DmabufFrame;

/// The returned texture owns the imported VkImage + VkDeviceMemory (and the
/// dup'd fd) and frees them when dropped. `None` on a non-linear frame or
/// any Vulkan failure (caller keeps using the SHM path).
pub fn import_dmabuf(device: &wgpu::Device, frame: &DmabufFrame) -> Option<wgpu::Texture> {
    if frame.modifier != 0 || frame.planes.len() != 1 {
        tracing::warn!(
            "dmabuf import needs a linear single-plane buffer (modifier {:#x}, {} planes); \
             set AMD_DEBUG=nodcc,notiling",
            frame.modifier,
            frame.planes.len()
        );
        return None;
    }
    // Vulkan takes ownership of the imported fd on success, so hand it a dup.
    let dup = frame.planes[0].fd.try_clone().ok()?;

    // AR24 = DRM_FORMAT_ARGB8888 = bytes B,G,R,A in memory = VK B8G8R8A8.
    let vk_format = vk::Format::B8G8R8A8_UNORM;
    let extent = vk::Extent3D { width: frame.width, height: frame.height, depth: 1 };

    let hal_texture = unsafe {
        let guard = device.as_hal::<vulkan::Api>()?;
        let hal_device: &vulkan::Device = &guard;
        let dev = hal_device.raw_device();
        let phys = hal_device.raw_physical_device();
        let instance = hal_device.shared_instance().raw_instance();

        let mut ext_ci = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let image_ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut ext_ci);
        let image = match dev.create_image(&image_ci, None) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("dmabuf import: vkCreateImage failed: {e:?}");
                return None;
            }
        };

        let reqs = dev.get_image_memory_requirements(image);
        let mem_type = match find_memory_type(instance, phys, reqs.memory_type_bits) {
            Some(t) => t,
            None => {
                dev.destroy_image(image, None);
                return None;
            }
        };

        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(dup.as_raw_fd());
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(mem_type)
            .push_next(&mut import_info)
            .push_next(&mut dedicated);
        let memory = match dev.allocate_memory(&alloc_info, None) {
            Ok(m) => {
                // vkAllocateMemory consumed the fd; don't let `dup` close it.
                let _ = dup.into_raw_fd();
                m
            }
            Err(e) => {
                tracing::warn!("dmabuf import: vkAllocateMemory failed: {e:?}");
                dev.destroy_image(image, None);
                return None; // `dup` drops → closes the fd
            }
        };

        if let Err(e) = dev.bind_image_memory(image, memory, 0) {
            tracing::warn!("dmabuf import: vkBindImageMemory failed: {e:?}");
            dev.free_memory(memory, None);
            dev.destroy_image(image, None);
            return None;
        }

        // Free the image + imported memory when the wgpu texture drops.
        let dev_owned = dev.clone();
        let drop_cb: vello::wgpu::hal::DropCallback = Box::new(move || {
            dev_owned.destroy_image(image, None);
            dev_owned.free_memory(memory, None);
        });
        let hal_desc = vello::wgpu::hal::TextureDescriptor {
            label: Some("webview-dmabuf"),
            size: wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: vello::wgpu::wgt::TextureUses::RESOURCE | vello::wgpu::wgt::TextureUses::COPY_SRC,
            memory_flags: vello::wgpu::hal::MemoryFlags::empty(),
            view_formats: vec![],
        };
        hal_device.texture_from_raw(
            image,
            &hal_desc,
            Some(drop_cb),
            vulkan::TextureMemory::External,
        )
    };

    let wgpu_desc = wgpu::TextureDescriptor {
        label: Some("webview-dmabuf"),
        size: wgpu::Extent3d {
            width: frame.width,
            height: frame.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    };
    Some(unsafe { device.create_texture_from_hal::<vulkan::Api>(hal_texture, &wgpu_desc) })
}

/// First memory type allowed by `type_bits` (imported dmabuf memory has no
/// host-visibility requirement, so any compatible type works).
fn find_memory_type(instance: &ash::Instance, phys: vk::PhysicalDevice, type_bits: u32) -> Option<u32> {
    let props = unsafe { instance.get_physical_device_memory_properties(phys) };
    (0..props.memory_type_count).find(|&i| type_bits & (1 << i) != 0)
}
