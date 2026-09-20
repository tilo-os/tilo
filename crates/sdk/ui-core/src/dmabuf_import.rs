//! DMA-BUF → `wgpu` texture import via Vulkan external memory.
//!
//! A producer that already has a picture in GPU memory — a hardware video
//! decoder, or WebKit rendering a page — can hand it over as a dmabuf: a file
//! descriptor naming the allocation, plus enough layout information to read
//! it. Importing that as a `wgpu::Texture` aliases the same memory, so nothing
//! is copied.
//!
//! # What is reachable, and what is not
//!
//! Import needs `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf`,
//! which `wgpu-hal` enables. Interpreting a **tiled or compressed** layout
//! additionally needs `VK_EXT_image_drm_format_modifier` — and `wgpu-hal` does
//! not enable it (verified against wgpu-hal 29: `adapter.rs` pushes
//! `external_memory_fd` and `external_memory_dma_buf`, and nothing else in
//! that family). Without it Vulkan can only be told `VK_IMAGE_TILING_LINEAR`,
//! so a buffer with any other DRM format modifier cannot be described and must
//! not be imported: doing so would sample the tiling pattern as pixels.
//!
//! Two further limits are enforced rather than hoped for:
//!
//! * **Vulkan only.** The import goes through `Device::as_hal::<vulkan::Api>`.
//!   If `wgpu` fell back to GLES there is no Vulkan device to import into, and
//!   [`ImportError::NotVulkan`] says so — the caller takes the CPU path
//!   instead of silently rendering nothing.
//! * **Row pitch must agree.** A `LINEAR` `VkImage`'s row pitch is chosen by
//!   the driver, and there is no way to override it without the modifier
//!   extension. If it disagrees with the producer's stride, the picture would
//!   shear; [`ImportError::StrideMismatch`] reports it and the caller falls
//!   back.
//!
//! Every failure is a typed error, never a silently blank frame.

use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd};

use ash::vk;
use vello::wgpu;
// `Device` brings `destroy_texture` into scope for the cleanup path below.
use vello::wgpu::hal::Device as _;
use vello::wgpu::hal::vulkan;

use media::dmabuf::{
    Colorimetry, DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, DRM_FORMAT_MOD_INVALID,
    DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_NV12, DRM_FORMAT_P010, DRM_FORMAT_XBGR8888,
    DRM_FORMAT_XRGB8888, DRM_FORMAT_YUYV, fourcc_str,
};

/// One plane of a buffer being imported, borrowed for the call.
#[derive(Clone, Copy)]
pub struct PlaneRef<'a> {
    pub fd: BorrowedFd<'a>,
    pub offset: u32,
    pub stride: u32,
}

/// A dmabuf described well enough to import, borrowing its descriptors.
///
/// Both `media::DmabufImage` (a decoded video picture) and the webview's
/// `DmabufFrame` (a rendered page) convert into this, so one importer serves
/// both.
pub struct DmabufRef<'a> {
    pub planes: &'a [PlaneRef<'a>],
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub colorimetry: Colorimetry,
}

/// Why an import could not happen. Each variant names a real, distinct
/// condition the caller can act on — all of them mean "use the CPU path".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// `wgpu` is not running on Vulkan (GLES fallback, or another backend), so
    /// there is no Vulkan device to import external memory into.
    NotVulkan,
    /// The buffer is tiled or compressed. Needs `VK_EXT_image_drm_format_modifier`,
    /// which `wgpu-hal` does not enable.
    UnsupportedModifier { modifier: u64 },
    /// No mapping from this DRM fourcc to a Vulkan format.
    UnsupportedFormat { fourcc: u32 },
    /// The plane count does not match what the format requires.
    PlaneCount {
        fourcc: u32,
        got: usize,
        expected: usize,
    },
    /// The driver's linear row pitch disagrees with the producer's stride, and
    /// without the modifier extension there is no way to reconcile them.
    StrideMismatch {
        plane: usize,
        producer: u32,
        driver: u64,
    },
    /// A Vulkan call failed.
    Vulkan(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::NotVulkan => write!(
                f,
                "wgpu is not on the Vulkan backend; dmabuf import needs Vulkan \
                 external memory, so the CPU path must be used"
            ),
            ImportError::UnsupportedModifier { modifier } => write!(
                f,
                "dmabuf modifier {modifier:#x} is tiled or compressed; importing it \
                 needs VK_EXT_image_drm_format_modifier, which wgpu-hal does not enable"
            ),
            ImportError::UnsupportedFormat { fourcc } => {
                write!(
                    f,
                    "no Vulkan mapping for DRM format {}",
                    fourcc_str(*fourcc)
                )
            }
            ImportError::PlaneCount {
                fourcc,
                got,
                expected,
            } => write!(
                f,
                "{} needs {expected} plane(s), got {got}",
                fourcc_str(*fourcc)
            ),
            ImportError::StrideMismatch {
                plane,
                producer,
                driver,
            } => write!(
                f,
                "plane {plane}: producer stride {producer} but the driver's linear \
                 row pitch is {driver}; the picture would shear"
            ),
            ImportError::Vulkan(e) => write!(f, "Vulkan: {e}"),
        }
    }
}

/// How the imported planes are laid out, and therefore how to sample them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneLayout {
    /// One texture holding RGBA or BGRA directly. Sample and use.
    Packed,
    /// Two textures: luma (`R8`/`R16`) and interleaved chroma (`Rg8`/`Rg16`)
    /// at half resolution. Needs a YUV→RGB conversion to display.
    BiPlanarYuv,
    /// One texture of 4:2:2 samples packed as `Y0 Cb Y1 Cr`, imported at half
    /// width so each texel holds two pixels' worth. Needs unpacking.
    PackedYuv422,
}

/// A dmabuf imported as sampleable textures.
pub struct ImportedPlanes {
    /// One entry for [`PlaneLayout::Packed`] and [`PlaneLayout::PackedYuv422`],
    /// two (luma then chroma) for [`PlaneLayout::BiPlanarYuv`].
    pub planes: Vec<wgpu::Texture>,
    pub layout: PlaneLayout,
    pub colorimetry: Colorimetry,
    pub width: u32,
    pub height: u32,
}

/// The Vulkan format and extent one plane of `fourcc` is imported as.
struct PlaneSpec {
    format: vk::Format,
    wgpu_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    /// Bytes per texel in the imported view, for the stride check.
    texel_bytes: u32,
}

/// How a DRM fourcc maps onto importable Vulkan planes.
///
/// Deliberately does **not** use Vulkan's multi-planar `G8_B8R8_2PLANE_420`
/// formats: sampling those needs a ycbcr conversion sampler, which `wgpu` does
/// not expose. Each plane is imported as an ordinary single-component image
/// instead and combined in a shader, which `wgpu` can express.
fn plane_specs(fourcc: u32, width: u32, height: u32) -> Option<(PlaneLayout, Vec<PlaneSpec>)> {
    // Chroma planes are half resolution, rounded up so odd dimensions keep a
    // whole chroma sample for the last row/column.
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    match fourcc {
        DRM_FORMAT_NV12 => Some((
            PlaneLayout::BiPlanarYuv,
            vec![
                PlaneSpec {
                    format: vk::Format::R8_UNORM,
                    wgpu_format: wgpu::TextureFormat::R8Unorm,
                    width,
                    height,
                    texel_bytes: 1,
                },
                PlaneSpec {
                    format: vk::Format::R8G8_UNORM,
                    wgpu_format: wgpu::TextureFormat::Rg8Unorm,
                    width: cw,
                    height: ch,
                    texel_bytes: 2,
                },
            ],
        )),
        DRM_FORMAT_P010 => Some((
            PlaneLayout::BiPlanarYuv,
            vec![
                PlaneSpec {
                    format: vk::Format::R16_UNORM,
                    wgpu_format: wgpu::TextureFormat::R16Unorm,
                    width,
                    height,
                    texel_bytes: 2,
                },
                PlaneSpec {
                    format: vk::Format::R16G16_UNORM,
                    wgpu_format: wgpu::TextureFormat::Rg16Unorm,
                    width: cw,
                    height: ch,
                    texel_bytes: 4,
                },
            ],
        )),
        // Y0 Cb Y1 Cr in four bytes: one RGBA texel per two pixels.
        DRM_FORMAT_YUYV => Some((
            PlaneLayout::PackedYuv422,
            vec![PlaneSpec {
                format: vk::Format::R8G8B8A8_UNORM,
                wgpu_format: wgpu::TextureFormat::Rgba8Unorm,
                width: cw,
                height,
                texel_bytes: 4,
            }],
        )),
        // DRM names these by their little-endian word order, so `AR24`
        // (ARGB8888) is B,G,R,A in memory — Vulkan's B8G8R8A8.
        DRM_FORMAT_ARGB8888 | DRM_FORMAT_XRGB8888 => Some((
            PlaneLayout::Packed,
            vec![PlaneSpec {
                format: vk::Format::B8G8R8A8_UNORM,
                wgpu_format: wgpu::TextureFormat::Bgra8Unorm,
                width,
                height,
                texel_bytes: 4,
            }],
        )),
        DRM_FORMAT_ABGR8888 | DRM_FORMAT_XBGR8888 => Some((
            PlaneLayout::Packed,
            vec![PlaneSpec {
                format: vk::Format::R8G8B8A8_UNORM,
                wgpu_format: wgpu::TextureFormat::Rgba8Unorm,
                width,
                height,
                texel_bytes: 4,
            }],
        )),
        _ => None,
    }
}

/// Whether a modifier describes a plain linear layout, the only one an import
/// without `VK_EXT_image_drm_format_modifier` can interpret.
fn is_linear(modifier: u64) -> bool {
    modifier == DRM_FORMAT_MOD_LINEAR || modifier == DRM_FORMAT_MOD_INVALID
}

/// Import `image` as sampleable `wgpu` textures, aliasing its GPU memory.
///
/// The returned textures own their `VkImage` + imported `VkDeviceMemory` (and
/// the duplicated fds) and release them when dropped. Releasing them is what
/// returns the buffer to the producer's pool, so do not hold them longer than
/// the GPU needs them.
///
/// # Errors
///
/// Returns [`ImportError`] when the buffer's format, modifier, or plane layout
/// is unsupported, the device is not backed by Vulkan, or Vulkan cannot import
/// one of the planes.
pub fn import(device: &wgpu::Device, image: &DmabufRef<'_>) -> Result<ImportedPlanes, ImportError> {
    if !is_linear(image.modifier) {
        return Err(ImportError::UnsupportedModifier {
            modifier: image.modifier,
        });
    }
    let (layout, specs) = plane_specs(image.fourcc, image.width, image.height).ok_or(
        ImportError::UnsupportedFormat {
            fourcc: image.fourcc,
        },
    )?;
    if image.planes.len() != specs.len() {
        return Err(ImportError::PlaneCount {
            fourcc: image.fourcc,
            got: image.planes.len(),
            expected: specs.len(),
        });
    }

    // Build the raw Vulkan images first, while the hal guard is held, then
    // wrap them as `wgpu::Texture`s once it is released.
    //
    // Safety: every handle created below is either moved into a hal texture
    // (whose drop callback frees it) or destroyed before returning an error.
    let hal_textures = {
        // Safety: the guard only reads the device's Vulkan handles, and is
        // dropped before any wgpu texture is created from them.
        let guard = unsafe { device.as_hal::<vulkan::Api>() }.ok_or(ImportError::NotVulkan)?;
        let hal_device: &vulkan::Device = &guard;
        let dev = hal_device.raw_device();
        let phys = hal_device.raw_physical_device();
        let instance = hal_device.shared_instance().raw_instance();
        let ext_fd = ash::khr::external_memory_fd::Device::new(instance, dev);

        let mut built = Vec::with_capacity(specs.len());
        for (i, (plane, spec)) in image.planes.iter().zip(&specs).enumerate() {
            // On failure, hal textures already built are dropped here without
            // ever reaching wgpu. Their drop callbacks are not run by `Drop`
            // (a hal texture is a plain handle), so free them explicitly.
            let made =
                unsafe { import_plane(dev, phys, instance, &ext_fd, hal_device, plane, spec, i) };
            match made {
                Ok(t) => built.push(t),
                Err(e) => {
                    for t in built {
                        unsafe { hal_device.destroy_texture(t) };
                    }
                    return Err(e);
                }
            }
        }
        built
    };

    let planes = hal_textures
        .into_iter()
        .zip(specs.iter())
        .map(|(hal, spec)| {
            let desc = wgpu::TextureDescriptor {
                label: Some("dmabuf-plane"),
                size: wgpu::Extent3d {
                    width: spec.width,
                    height: spec.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: spec.wgpu_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            };
            // Safety: `hal` was created from this same device moments ago and
            // has not been used since.
            unsafe { device.create_texture_from_hal::<vulkan::Api>(hal, &desc) }
        })
        .collect();

    Ok(ImportedPlanes {
        planes,
        layout,
        colorimetry: image.colorimetry,
        width: image.width,
        height: image.height,
    })
}

/// Import one plane as a `wgpu::Texture`.
#[allow(clippy::too_many_arguments)]
// Keeping acquisition and cleanup together makes the ownership transfer to
// Vulkan auditable; splitting this sequence would obscure its early returns.
#[allow(clippy::too_many_lines)]
unsafe fn import_plane(
    dev: &ash::Device,
    phys: vk::PhysicalDevice,
    instance: &ash::Instance,
    ext_fd: &ash::khr::external_memory_fd::Device,
    hal_device: &vulkan::Device,
    plane: &PlaneRef<'_>,
    spec: &PlaneSpec,
    index: usize,
) -> Result<<vulkan::Api as vello::wgpu::hal::Api>::Texture, ImportError> {
    // Vulkan takes ownership of an imported fd on success, so hand it a
    // duplicate and leave the caller's original alone.
    let dup = plane
        .fd
        .try_clone_to_owned()
        .map_err(|e| ImportError::Vulkan(format!("dup dmabuf fd: {e}")))?;

    let mut ext_ci = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let image_ci = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(spec.format)
        .extent(vk::Extent3D {
            width: spec.width,
            height: spec.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        // The only tiling describable without VK_EXT_image_drm_format_modifier.
        .tiling(vk::ImageTiling::LINEAR)
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut ext_ci);

    let vk_image = unsafe { dev.create_image(&image_ci, None) }
        .map_err(|e| ImportError::Vulkan(format!("vkCreateImage: {e:?}")))?;

    // Guard so every early return frees the image.
    let cleanup_image = |dev: &ash::Device| unsafe { dev.destroy_image(vk_image, None) };

    // A LINEAR image's row pitch is the driver's choice; there is no way to
    // impose the producer's without the modifier extension. If they disagree
    // the sampled picture shears, so refuse rather than render garbage.
    let sub = vk::ImageSubresource {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        array_layer: 0,
    };
    let sub_layout = unsafe { dev.get_image_subresource_layout(vk_image, sub) };
    if sub_layout.row_pitch != u64::from(plane.stride) {
        // A stride that is merely *larger* is still unusable: the sampler
        // steps by the driver's pitch, so rows would drift.
        cleanup_image(dev);
        return Err(ImportError::StrideMismatch {
            plane: index,
            producer: plane.stride,
            driver: sub_layout.row_pitch,
        });
    }
    debug_assert!(spec.texel_bytes > 0);

    let reqs = unsafe { dev.get_image_memory_requirements(vk_image) };

    // Which memory types the *imported fd* is compatible with. Intersecting
    // this with the image's requirements is the correct way to choose; picking
    // the first bit of the image's mask alone can select a type the imported
    // allocation does not support.
    let fd_props = {
        let mut props = vk::MemoryFdPropertiesKHR::default();
        match unsafe {
            ext_fd.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                dup.as_raw_fd(),
                &mut props,
            )
        } {
            Ok(()) => props.memory_type_bits,
            // Not fatal: fall back to the image's own mask.
            Err(_) => u32::MAX,
        }
    };
    let allowed = reqs.memory_type_bits & fd_props;
    let Some(mem_type) = find_memory_type(instance, phys, allowed) else {
        cleanup_image(dev);
        return Err(ImportError::Vulkan(format!(
            "no memory type accepts the imported dmabuf (image mask {:#x}, fd mask {fd_props:#x})",
            reqs.memory_type_bits
        )));
    };

    let mut import_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(dup.as_raw_fd());
    // A dedicated allocation binds at offset 0 only. Planes that live at an
    // offset inside a shared buffer need a plain allocation instead.
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(vk_image);
    let use_dedicated = plane.offset == 0;
    // The allocation must cover the plane's offset as well as its size.
    let Some(size) = reqs.size.checked_add(u64::from(plane.offset)) else {
        cleanup_image(dev);
        return Err(ImportError::Vulkan(format!(
            "dmabuf allocation size overflows (image size {}, plane offset {})",
            reqs.size, plane.offset
        )));
    };

    let mut alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(size)
        .memory_type_index(mem_type)
        .push_next(&mut import_info);
    if use_dedicated {
        alloc_info = alloc_info.push_next(&mut dedicated);
    }

    let memory = match unsafe { dev.allocate_memory(&alloc_info, None) } {
        Ok(m) => {
            // vkAllocateMemory consumed the fd; don't let `dup` close it.
            let _ = dup.into_raw_fd();
            m
        }
        Err(e) => {
            cleanup_image(dev);
            // `dup` drops here and closes the fd.
            return Err(ImportError::Vulkan(format!("vkAllocateMemory: {e:?}")));
        }
    };

    if let Err(e) = unsafe { dev.bind_image_memory(vk_image, memory, u64::from(plane.offset)) } {
        unsafe { dev.free_memory(memory, None) };
        cleanup_image(dev);
        return Err(ImportError::Vulkan(format!("vkBindImageMemory: {e:?}")));
    }

    // Free the image + imported memory when the wgpu texture drops.
    let dev_owned = dev.clone();
    let drop_cb: vello::wgpu::hal::DropCallback = Box::new(move || unsafe {
        dev_owned.destroy_image(vk_image, None);
        dev_owned.free_memory(memory, None);
    });
    let size3d = wgpu::Extent3d {
        width: spec.width,
        height: spec.height,
        depth_or_array_layers: 1,
    };
    let hal_desc = vello::wgpu::hal::TextureDescriptor {
        label: Some("dmabuf-plane"),
        size: size3d,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: spec.wgpu_format,
        usage: vello::wgpu::wgt::TextureUses::RESOURCE | vello::wgpu::wgt::TextureUses::COPY_SRC,
        memory_flags: vello::wgpu::hal::MemoryFlags::empty(),
        view_formats: vec![],
    };
    Ok(unsafe {
        hal_device.texture_from_raw(
            vk_image,
            &hal_desc,
            Some(drop_cb),
            vulkan::TextureMemory::External,
        )
    })
}

/// First memory type allowed by `type_bits`. Imported dmabuf memory carries no
/// host-visibility requirement, so any compatible type works.
fn find_memory_type(
    instance: &ash::Instance,
    phys: vk::PhysicalDevice,
    type_bits: u32,
) -> Option<u32> {
    if type_bits == 0 {
        return None;
    }
    let props = unsafe { instance.get_physical_device_memory_properties(phys) };
    (0..props.memory_type_count).find(|&i| type_bits & (1 << i) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tiled_modifier_is_refused_rather_than_sampled_as_pixels() {
        // The AMD GFX9 tiled modifier a VA-API decoder exports. Importing it
        // as linear would read the tiling pattern as picture content.
        assert!(!is_linear(0x0200_0000_1040_1b04));
        assert!(is_linear(DRM_FORMAT_MOD_LINEAR));
        assert!(
            is_linear(DRM_FORMAT_MOD_INVALID),
            "unstated is treated as linear"
        );
    }

    #[test]
    fn nv12_maps_to_a_luma_and_a_half_resolution_chroma_plane() {
        let (layout, specs) = plane_specs(DRM_FORMAT_NV12, 1920, 1080).unwrap();
        assert_eq!(layout, PlaneLayout::BiPlanarYuv);
        assert_eq!(specs.len(), 2);
        assert_eq!((specs[0].width, specs[0].height), (1920, 1080));
        assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::R8Unorm);
        assert_eq!(
            (specs[1].width, specs[1].height),
            (960, 540),
            "chroma is half in both axes"
        );
        assert_eq!(
            specs[1].wgpu_format,
            wgpu::TextureFormat::Rg8Unorm,
            "NV12 chroma is interleaved Cb,Cr — two components in one plane"
        );
    }

    #[test]
    fn p010_uses_sixteen_bit_planes_for_its_ten_bit_samples() {
        let (layout, specs) = plane_specs(DRM_FORMAT_P010, 3840, 2160).unwrap();
        assert_eq!(layout, PlaneLayout::BiPlanarYuv);
        assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::R16Unorm);
        assert_eq!(specs[1].wgpu_format, wgpu::TextureFormat::Rg16Unorm);
        assert_eq!(specs[0].texel_bytes, 2);
        assert_eq!(specs[1].texel_bytes, 4);
    }

    #[test]
    fn odd_dimensions_keep_a_whole_chroma_sample_for_the_last_row_and_column() {
        // Rounding down would drop the right column and bottom row of chroma,
        // tinting the edges of an odd-sized picture.
        let (_, specs) = plane_specs(DRM_FORMAT_NV12, 1919, 1079).unwrap();
        assert_eq!((specs[1].width, specs[1].height), (960, 540));
    }

    #[test]
    fn yuyv_is_imported_at_half_width_because_one_texel_holds_two_pixels() {
        let (layout, specs) = plane_specs(DRM_FORMAT_YUYV, 1920, 1080).unwrap();
        assert_eq!(layout, PlaneLayout::PackedYuv422);
        assert_eq!(specs.len(), 1);
        assert_eq!((specs[0].width, specs[0].height), (960, 1080));
        assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn drm_rgb_names_map_to_the_matching_vulkan_byte_order() {
        // Getting this backwards swaps red and blue — the classic dmabuf bug.
        // DRM names by little-endian word order, so AR24 (ARGB8888) is
        // B,G,R,A in memory.
        let (layout, specs) = plane_specs(DRM_FORMAT_ARGB8888, 64, 64).unwrap();
        assert_eq!(layout, PlaneLayout::Packed);
        assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(specs[0].format, vk::Format::B8G8R8A8_UNORM);

        let (_, specs) = plane_specs(DRM_FORMAT_ABGR8888, 64, 64).unwrap();
        assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::Rgba8Unorm);

        // The X variants share the layout; alpha is simply ignored.
        let (_, x) = plane_specs(DRM_FORMAT_XRGB8888, 64, 64).unwrap();
        assert_eq!(x[0].wgpu_format, wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn an_unknown_fourcc_is_rejected_rather_than_guessed_at() {
        assert!(plane_specs(media::dmabuf::fourcc(b'Z', b'Z', b'Z', b'Z'), 64, 64).is_none());
    }

    #[test]
    fn every_error_says_what_to_do_about_it() {
        // These strings end up in a log when a frame does not appear; a bare
        // "import failed" would leave nothing to act on.
        let cases = [
            ImportError::NotVulkan,
            ImportError::UnsupportedModifier {
                modifier: 0x0200_0000_1040_1b04,
            },
            ImportError::UnsupportedFormat {
                fourcc: DRM_FORMAT_NV12,
            },
            ImportError::PlaneCount {
                fourcc: DRM_FORMAT_NV12,
                got: 1,
                expected: 2,
            },
            ImportError::StrideMismatch {
                plane: 0,
                producer: 1920,
                driver: 2048,
            },
        ];
        for e in cases {
            let s = e.to_string();
            assert!(s.len() > 20, "error text is too terse to act on: {s:?}");
            assert!(!s.contains("{"), "unformatted placeholder in {s:?}");
        }
        assert!(
            ImportError::NotVulkan.to_string().contains("CPU path"),
            "the GLES fallback must say what the caller should do instead"
        );
        assert!(
            ImportError::UnsupportedModifier { modifier: 1 }
                .to_string()
                .contains("VK_EXT_image_drm_format_modifier"),
            "name the missing extension so the limitation is diagnosable"
        );
    }
}
