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
//! which `wgpu-hal` enables. Interpreting a **tiled or compressed** layout, or
//! a linear one whose row pitch the driver would not have chosen itself,
//! additionally needs `VK_EXT_image_drm_format_modifier`. The crates.io
//! `wgpu-hal` 29 does not enable it; the repository's patched copy
//! (`vendor/wgpu-hal`, see `vendor/PATCHES.md`) does on Vulkan 1.2 and up.
//!
//! * With the extension ([`explicit_layout_available`]) the image is created
//!   with the producer's own modifier, plane offset and row pitch
//!   (`VkImageDrmFormatModifierExplicitCreateInfoEXT`), after the driver
//!   confirmed it can sample that exact combination. No pitch has to agree.
//! * Without it Vulkan can only be told `VK_IMAGE_TILING_LINEAR`, so a buffer
//!   with any other DRM format modifier cannot be described and must not be
//!   imported: doing so would sample the tiling pattern as pixels.
//!
//! Two further limits are enforced rather than hoped for:
//!
//! * **Vulkan only.** The import goes through `Device::as_hal::<vulkan::Api>`.
//!   If `wgpu` fell back to GLES there is no Vulkan device to import into, and
//!   [`ImportError::NotVulkan`] says so — the caller takes the CPU path
//!   instead of silently rendering nothing.
//! * **Row pitch must agree, without the modifier extension.** A `LINEAR`
//!   `VkImage`'s row pitch is chosen by the driver, and there is no way to
//!   override it without the extension. If it disagrees with the producer's stride, the picture would
//!   shear; [`ImportError::StrideMismatch`] reports it and the caller falls
//!   back.
//!
//! Every failure is a typed error, never a silently blank frame.
//!
//! [`ImportError::NotVulkan`]: crate::dmabuf_import::ImportError::NotVulkan
//! [`ImportError::StrideMismatch`]: crate::dmabuf_import::ImportError::StrideMismatch

use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd};
use std::sync::Arc;

use ash::vk;
use vello::wgpu;
// `Device` brings `destroy_texture` into scope for the cleanup path below.
use vello::wgpu::hal::Device as _;
use vello::wgpu::hal::vulkan;

use media::dmabuf::{
    Colorimetry, DRM_FORMAT_ABGR8888, DRM_FORMAT_ABGR2101010, DRM_FORMAT_ABGR16161616F,
    DRM_FORMAT_ARGB8888, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_NV12,
    DRM_FORMAT_P010, DRM_FORMAT_XBGR8888, DRM_FORMAT_XBGR2101010, DRM_FORMAT_XBGR16161616F,
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
/// A `media::DmabufImage` (a decoded video picture) converts into this.
pub struct DmabufRef<'a> {
    pub planes: &'a [PlaneRef<'a>],
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub colorimetry: Colorimetry,
}

/// DRM character-device identities reported by the Vulkan physical device.
///
/// `VK_EXT_physical_device_drm` is the authoritative Linux identity for
/// matching a Vulkan renderer to an open KMS/render node.  Unlike PCI bus
/// information it is also available for virtual DRM devices such as virgl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VulkanDrmDeviceIdentity {
    pub primary: Option<(u32, u32)>,
    pub render: Option<(u32, u32)>,
}

/// Query the DRM nodes owned by the exact Vulkan physical device behind
/// `device`. Returns `None` when the backend is not Vulkan or the driver does
/// not expose `VK_EXT_physical_device_drm`.
pub fn vulkan_drm_device_identity(device: &wgpu::Device) -> Option<VulkanDrmDeviceIdentity> {
    let guard = unsafe { device.as_hal::<vulkan::Api>() }?;
    let hal_device: &vulkan::Device = &guard;
    let phys = hal_device.raw_physical_device();
    let instance = hal_device.shared_instance().raw_instance();
    let supported = unsafe { instance.enumerate_device_extension_properties(phys).ok()? }
        .iter()
        .any(|extension| {
            extension.extension_name_as_c_str().ok() == Some(ash::ext::physical_device_drm::NAME)
        });
    if !supported {
        return None;
    }

    let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
    unsafe { instance.get_physical_device_properties2(phys, &mut properties) };
    let node = |present: u32, major: i64, minor: i64| {
        (present != 0)
            .then(|| Some((u32::try_from(major).ok()?, u32::try_from(minor).ok()?)))
            .flatten()
    };
    Some(VulkanDrmDeviceIdentity {
        primary: node(drm.has_primary, drm.primary_major, drm.primary_minor),
        render: node(drm.has_render, drm.render_major, drm.render_minor),
    })
}

/// Why an import could not happen. Each variant names a real, distinct
/// condition the caller can act on — all of them mean "use the CPU path".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// `wgpu` is not running on Vulkan (GLES fallback, or another backend), so
    /// there is no Vulkan device to import external memory into.
    NotVulkan,
    /// The buffer is tiled or compressed and the device cannot sample it:
    /// `VK_EXT_image_drm_format_modifier` is not enabled (the crates.io
    /// `wgpu-hal` does not), or the driver lists no such modifier for the
    /// format.
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
                 needs VK_EXT_image_drm_format_modifier enabled on the device and \
                 a driver that can sample the modifier"
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

/// One compositor-owned scanout dma-buf imported as a writable render target.
///
/// This is intentionally separate from [`ImportedPlanes`].  Video and WebView
/// imports are sampled by the compositor; this texture is the final GPU
/// destination that KMS scans out.  Mixing those contracts would make it easy
/// to request the wrong Vulkan/wgpu usages and only discover that as a blank
/// physical display.
pub struct ImportedRenderTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// `Bgra8Unorm` for an SDR `XRGB8888` target; `Rgb10a2Unorm` or
    /// `Rgba16Float` for an HDR one.
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

/// The scanout formats the compositor renders into, with the Vulkan and wgpu
/// formats that describe the same bytes and the bytes per pixel.
///
/// DRM names formats by little-endian word order: `XR24` is B,G,R,X in
/// memory (Vulkan `B8G8R8A8`), `XB30` packs red into the low ten bits of a
/// word (Vulkan `A2B10G10R10_PACK32`, wgpu `Rgb10a2Unorm`), and `AB4H` stores
/// half-float red first (Vulkan `R16G16B16A16_SFLOAT`).
pub fn render_target_format(fourcc: u32) -> Option<(vk::Format, wgpu::TextureFormat, u32)> {
    match fourcc {
        DRM_FORMAT_XRGB8888 => Some((
            vk::Format::B8G8R8A8_UNORM,
            wgpu::TextureFormat::Bgra8Unorm,
            4,
        )),
        DRM_FORMAT_XBGR2101010 => Some((
            vk::Format::A2B10G10R10_UNORM_PACK32,
            wgpu::TextureFormat::Rgb10a2Unorm,
            4,
        )),
        DRM_FORMAT_ABGR16161616F => Some((
            vk::Format::R16G16B16A16_SFLOAT,
            wgpu::TextureFormat::Rgba16Float,
            8,
        )),
        _ => None,
    }
}

/// Import the one linear plane of a compositor-owned scanout buffer
/// (`XRGB8888`, `XBGR2101010` or `ABGR16161616F`) as a render attachment on
/// `device`.
///
/// The allocation remains owned by the caller (normally a GBM buffer object).
/// The returned Vulkan image and memory import are aliases and must be dropped
/// before that allocation.  Import once per scanout slot, never once per frame.
///
/// # Errors
///
/// Fails closed for every layout which cannot be described exactly without
/// `VK_EXT_image_drm_format_modifier`: other formats, more than one plane,
/// non-zero plane offsets, tiled/compressed modifiers, or a driver-selected
/// row pitch that differs from the producer's stride.
pub fn import_render_target(
    device: &wgpu::Device,
    image: &DmabufRef<'_>,
) -> Result<ImportedRenderTarget, ImportError> {
    let (format, wgpu_format, texel_bytes) = validate_render_target(image)?;

    let plane = image.planes[0];
    let spec = PlaneSpec {
        format,
        wgpu_format,
        width: image.width,
        height: image.height,
        texel_bytes,
    };
    let hal = {
        // Safety: the guard only lends this device's Vulkan handles.  The raw
        // texture is wrapped by the same wgpu device immediately afterward.
        let guard = unsafe { device.as_hal::<vulkan::Api>() }.ok_or(ImportError::NotVulkan)?;
        let hal_device: &vulkan::Device = &guard;
        unsafe {
            VulkanImporter::new(hal_device).import_plane_with_usage(
                &plane,
                &spec,
                0,
                PlaneUsage {
                    modifier: None,
                    vk: vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    hal: vello::wgpu::wgt::TextureUses::COLOR_TARGET,
                    label: "dmabuf-scanout-target",
                },
            )
        }?
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("dmabuf-scanout-target"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    };
    // Safety: `hal` was created from `device`, with a descriptor exactly
    // matching `desc`, and ownership is transferred into this texture.
    let texture = unsafe { device.create_texture_from_hal::<vulkan::Api>(hal, &desc) };
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(ImportedRenderTarget {
        texture,
        view,
        format: wgpu_format,
        width: image.width,
        height: image.height,
        stride: plane.stride,
    })
}

fn validate_render_target(
    image: &DmabufRef<'_>,
) -> Result<(vk::Format, wgpu::TextureFormat, u32), ImportError> {
    let Some((format, wgpu_format, texel_bytes)) = render_target_format(image.fourcc) else {
        return Err(ImportError::UnsupportedFormat {
            fourcc: image.fourcc,
        });
    };
    if !is_linear(image.modifier) {
        return Err(ImportError::UnsupportedModifier {
            modifier: image.modifier,
        });
    }
    if image.planes.len() != 1 {
        return Err(ImportError::PlaneCount {
            fourcc: image.fourcc,
            got: image.planes.len(),
            expected: 1,
        });
    }
    if image.width == 0 || image.height == 0 {
        return Err(ImportError::Vulkan(format!(
            "scanout extent must be non-zero, got {}x{}",
            image.width, image.height
        )));
    }
    let plane = image.planes[0];
    if plane.offset != 0 {
        return Err(ImportError::Vulkan(format!(
            "scanout plane offset must be zero, got {}",
            plane.offset
        )));
    }
    let minimum_stride = image
        .width
        .checked_mul(texel_bytes)
        .ok_or_else(|| ImportError::Vulkan("scanout row size overflows u32".to_owned()))?;
    if plane.stride < minimum_stride {
        return Err(ImportError::StrideMismatch {
            plane: 0,
            producer: plane.stride,
            driver: u64::from(minimum_stride),
        });
    }

    Ok((format, wgpu_format, texel_bytes))
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
        // 10-bit RGB, red in the low bits of a little-endian word.
        DRM_FORMAT_ABGR2101010 | DRM_FORMAT_XBGR2101010 => Some((
            PlaneLayout::Packed,
            vec![PlaneSpec {
                format: vk::Format::A2B10G10R10_UNORM_PACK32,
                wgpu_format: wgpu::TextureFormat::Rgb10a2Unorm,
                width,
                height,
                texel_bytes: 4,
            }],
        )),
        DRM_FORMAT_ABGR16161616F | DRM_FORMAT_XBGR16161616F => Some((
            PlaneLayout::Packed,
            vec![PlaneSpec {
                format: vk::Format::R16G16B16A16_SFLOAT,
                wgpu_format: wgpu::TextureFormat::Rgba16Float,
                width,
                height,
                texel_bytes: 8,
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

/// Whether `device` was opened with `VK_EXT_image_drm_format_modifier`, so an
/// import can state the producer's exact layout instead of assuming the
/// driver's linear one.
pub fn explicit_layout_available(device: &wgpu::Device) -> bool {
    // Safety: the guard only lends this device's Vulkan handles for the call.
    unsafe { device.as_hal::<vulkan::Api>() }.is_some_and(|hal| has_modifier_extension(&hal))
}

fn has_modifier_extension(hal_device: &vulkan::Device) -> bool {
    hal_device
        .enabled_device_extensions()
        .contains(&ash::ext::image_drm_format_modifier::NAME)
}

/// The DRM modifiers `device` can sample for a single-plane dma-buf of
/// `fourcc`: each one the driver lists for the format and confirms
/// ([`VulkanImporter::check_modifier`], the check every import repeats at its
/// real size) can be imported and sampled. Empty when the device is not
/// Vulkan, lacks `VK_EXT_image_drm_format_modifier`, or has no such format.
///
/// A compositor advertises exactly these (beyond the linear layout it reads
/// on the CPU), so a client can only pick a layout the import will accept.
pub fn sampleable_modifiers(device: &wgpu::Device, fourcc: u32) -> Vec<u64> {
    // Safety: the guard only lends this device's Vulkan handles for the call.
    let Some(guard) = (unsafe { device.as_hal::<vulkan::Api>() }) else {
        return Vec::new();
    };
    let hal_device: &vulkan::Device = &guard;
    if !has_modifier_extension(hal_device) {
        return Vec::new();
    }
    let Some((PlaneLayout::Packed, specs)) = plane_specs(fourcc, 1, 1) else {
        return Vec::new();
    };
    let [spec] = specs.as_slice() else {
        return Vec::new();
    };
    let importer = VulkanImporter::new(hal_device);
    let usage = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC;
    importer
        .listed_modifiers(spec.format)
        .iter()
        .map(|props| props.drm_format_modifier)
        .filter(|&modifier| importer.check_modifier(spec, modifier, usage).is_ok())
        .collect()
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
    if !is_linear(image.modifier) && !explicit_layout_available(device) {
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
        let importer = VulkanImporter::new(hal_device);

        let mut built = Vec::with_capacity(specs.len());
        for (i, (plane, spec)) in image.planes.iter().zip(&specs).enumerate() {
            // On failure, hal textures already built are dropped here without
            // ever reaching wgpu. Their drop callbacks are not run by `Drop`
            // (a hal texture is a plain handle), so free them explicitly.
            let made = unsafe { importer.import_plane(plane, spec, i, image.modifier) };
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

/// Per-commit images created and not yet destroyed, and what making them has
/// cost, since the process started.
static LIVE_IMAGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static WRAPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WRAP_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `(images alive, images made, nanoseconds spent making them)` for the
/// per-commit images of [`DmabufImage::texture`]. An image is alive from its
/// creation until wgpu destroys its texture, which it does only after every
/// submission that used it retired, so `alive` is the GPU's backlog.
pub fn image_stats() -> (usize, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        LIVE_IMAGES.load(Relaxed),
        WRAPS.load(Relaxed),
        WRAP_NANOS.load(Relaxed),
    )
}

/// A client's single-plane dma-buf, imported once, that yields a texture to
/// draw each commit from ([`Self::texture`]).
///
/// A foreign producer writes the buffer again between commits, and the spec
/// has an image acquire it from `VK_QUEUE_FAMILY_FOREIGN_EXT` after each
/// write, which a texture wgpu keeps in one layout cannot. So with explicit
/// layouts the device memory is imported once and every commit binds it to a
/// fresh `VkImage`, whose first use is that acquire (a patched
/// `wgpu-hal` makes the first barrier of a [`ForeignExternal`] texture one).
/// An image over memory costs a `vkCreateImage` and a bind, no import.
///
/// [`ForeignExternal`]: vulkan::TextureMemory::ForeignExternal
#[derive(Clone)]
pub struct DmabufImage(ImageKind);

#[derive(Clone)]
enum ImageKind {
    /// A device without explicit layouts imports linear buffers as one
    /// texture, reused by every commit.
    Fixed(wgpu::Texture),
    /// The memory imported once; an image over it per commit.
    Aliased(Arc<AliasedImage>),
}

/// An imported dma-buf's memory, freed after the last image over it.
struct AliasedMemory {
    dev: ash::Device,
    memory: vk::DeviceMemory,
}

impl Drop for AliasedMemory {
    fn drop(&mut self) {
        // Safety: every image over it is destroyed first (each holds an
        // `Arc` to this), and wgpu runs that only after the last submission
        // using the image retired.
        unsafe { self.dev.free_memory(self.memory, None) };
    }
}

/// What it takes to make another image over an imported dma-buf's memory.
struct AliasedImage {
    memory: Arc<AliasedMemory>,
    format: vk::Format,
    wgpu_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    modifier: u64,
    offset: u32,
    stride: u32,
}

/// The usage every imported client image is created and used with.
const SAMPLED_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw() | vk::ImageUsageFlags::TRANSFER_SRC.as_raw(),
);

/// `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`: `_IOWR('b', 2, struct dma_buf_export_sync_file)`.
const DMA_BUF_IOCTL_EXPORT_SYNC_FILE: libc::c_ulong = 0xC008_6202;
/// `DMA_BUF_SYNC_READ`: the fences a reader must wait for, the writers'.
const DMA_BUF_SYNC_READ: u32 = 1;

/// Submissions made to wait for a client buffer's pending writes since the
/// process started ([`wait_for_writes`]).
static WRITE_FENCE_WAITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times a submission was made to wait on the GPU for a client
/// buffer's pending writes since the process started: commits whose buffer
/// the producer's GPU had not finished when it was imported.
pub fn write_fence_waits() -> u64 {
    WRITE_FENCE_WAITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// `struct dma_buf_export_sync_file`.
#[repr(C)]
struct DmaBufExportSyncFile {
    flags: u32,
    fd: i32,
}

/// A client buffer's pending writes, as a semaphore the presenting queue's
/// next submission waits for ([`wait_for_writes`]). Keep it until a
/// submission made after it completed; dropping it destroys the semaphore.
pub struct WriteFence {
    device: wgpu::Device,
    semaphore: vk::Semaphore,
}

impl std::fmt::Debug for WriteFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteFence").finish_non_exhaustive()
    }
}

impl Drop for WriteFence {
    fn drop(&mut self) {
        // Safety: the guard only lends this device's Vulkan handles; the
        // semaphore is this device's, and its owner kept it until the
        // submission that waited for it completed.
        if let Some(guard) = unsafe { self.device.as_hal::<vulkan::Api>() } {
            unsafe { guard.raw_device().destroy_semaphore(self.semaphore, None) };
        }
    }
}

/// The sync file of the writes still pending on a dma-buf plane: the fence a
/// reader waits for. `None` when the kernel cannot export it (before Linux
/// 6.0) or nothing is pending.
fn export_write_fence(plane: BorrowedFd<'_>) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd as _;
    let mut request = DmaBufExportSyncFile {
        flags: DMA_BUF_SYNC_READ,
        fd: -1,
    };
    // Safety: the ioctl reads `flags` and writes `fd` of the struct it is
    // handed, which outlives the call; the dma-buf fd is borrowed for it.
    let result = unsafe {
        libc::ioctl(
            plane.as_raw_fd(),
            DMA_BUF_IOCTL_EXPORT_SYNC_FILE,
            &raw mut request,
        )
    };
    if result != 0 || request.fd < 0 {
        return None;
    }
    // Safety: the kernel returned a new sync file descriptor this owns.
    let fence = unsafe { std::os::fd::OwnedFd::from_raw_fd(request.fd) };
    let mut poll = libc::pollfd {
        fd: fence.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // A signalled fence (the writes are done) needs no wait. Never blocks.
    // Safety: one valid pollfd, borrowed for the call.
    let signalled = unsafe { libc::poll(&raw mut poll, 1, 0) } == 1;
    (!signalled).then_some(fence)
}

/// Make the next submission on `queue` wait on the GPU for the writes still
/// pending on the single-plane dma-buf `plane` (the producer's implicit
/// fence), so a sample of the buffer in it or any later submission reads what
/// the producer wrote. No thread waits: the fence is exported as a sync file
/// and imported as a semaphore the submission waits for.
///
/// `Ok(None)` when nothing is pending, or the kernel cannot export the fence
/// (the caller then relies on having watched the buffer become readable).
///
/// # Errors
///
/// [`ImportError::NotVulkan`], or [`ImportError::Vulkan`] when the device
/// lacks `VK_KHR_external_semaphore_fd` or the semaphore cannot be made.
pub fn wait_for_writes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    plane: BorrowedFd<'_>,
) -> Result<Option<WriteFence>, ImportError> {
    let Some(fence) = export_write_fence(plane) else {
        return Ok(None);
    };
    // Safety: the guards only lend this device's and queue's Vulkan handles
    // for the call.
    let guard = unsafe { device.as_hal::<vulkan::Api>() }.ok_or(ImportError::NotVulkan)?;
    let hal_device: &vulkan::Device = &guard;
    if !hal_device
        .enabled_device_extensions()
        .contains(&ash::khr::external_semaphore_fd::NAME)
    {
        return Err(ImportError::Vulkan(
            "VK_KHR_external_semaphore_fd is not enabled".to_owned(),
        ));
    }
    let instance = hal_device.shared_instance().raw_instance();
    let dev = hal_device.raw_device();
    let semaphore = unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
        .map_err(|error| ImportError::Vulkan(format!("vkCreateSemaphore: {error}")))?;
    let fd = fence.into_raw_fd();
    let info = vk::ImportSemaphoreFdInfoKHR::default()
        .semaphore(semaphore)
        .flags(vk::SemaphoreImportFlags::TEMPORARY)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
        .fd(fd);
    // Safety: a new semaphore of this device; on success the driver owns the
    // sync file descriptor.
    let imported = unsafe {
        ash::khr::external_semaphore_fd::Device::new(instance, dev).import_semaphore_fd(&info)
    };
    if let Err(error) = imported {
        // Safety: the import failed, so the descriptor is still ours to close.
        drop(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
        unsafe { dev.destroy_semaphore(semaphore, None) };
        return Err(ImportError::Vulkan(format!(
            "vkImportSemaphoreFdKHR (sync file): {error}"
        )));
    }
    let queue_guard = unsafe { queue.as_hal::<vulkan::Api>() }.ok_or(ImportError::NotVulkan);
    let Ok(queue_guard) = queue_guard else {
        unsafe { dev.destroy_semaphore(semaphore, None) };
        return Err(ImportError::NotVulkan);
    };
    queue_guard.add_wait_semaphore(semaphore, vk::PipelineStageFlags::ALL_COMMANDS);
    WRITE_FENCE_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    drop(queue_guard);
    drop(guard);
    Ok(Some(WriteFence {
        device: device.clone(),
        semaphore,
    }))
}

/// Import `image`, a single plane, so that each commit can draw from it
/// ([`DmabufImage::texture`]).
///
/// # Errors
///
/// As [`import`], and [`ImportError::UnsupportedModifier`] when the driver
/// does not list the layout as sampleable.
pub fn import_image(
    device: &wgpu::Device,
    image: &DmabufRef<'_>,
) -> Result<DmabufImage, ImportError> {
    let single = |image: &DmabufRef<'_>| -> Result<DmabufImage, ImportError> {
        let planes = import(device, image)?.planes;
        let got = planes.len();
        planes
            .into_iter()
            .next()
            .map(|texture| DmabufImage(ImageKind::Fixed(texture)))
            .ok_or(ImportError::PlaneCount {
                fourcc: image.fourcc,
                got,
                expected: 1,
            })
    };
    if !explicit_layout_available(device) || image.planes.len() != 1 {
        return single(image);
    }
    let (_, specs) = plane_specs(image.fourcc, image.width, image.height).ok_or(
        ImportError::UnsupportedFormat {
            fourcc: image.fourcc,
        },
    )?;
    let ([spec], [plane]) = (specs.as_slice(), image.planes) else {
        return single(image);
    };
    // Safety: the guard only lends this device's Vulkan handles for the call.
    let guard = unsafe { device.as_hal::<vulkan::Api>() }.ok_or(ImportError::NotVulkan)?;
    let hal_device: &vulkan::Device = &guard;
    let aliased =
        unsafe { VulkanImporter::new(hal_device).import_aliased(plane, spec, image.modifier) }?;
    Ok(DmabufImage(ImageKind::Aliased(Arc::new(aliased))))
}

impl DmabufImage {
    /// A texture of the buffer as it is now, to draw one commit from. Its
    /// first use acquires the image from the foreign queue family, so the
    /// producer's latest writes and compression metadata are what is read.
    ///
    /// # Errors
    ///
    /// [`ImportError::Vulkan`] when the image cannot be created or bound.
    pub fn texture(&self, device: &wgpu::Device) -> Result<wgpu::Texture, ImportError> {
        let aliased = match &self.0 {
            ImageKind::Fixed(texture) => return Ok(texture.clone()),
            ImageKind::Aliased(aliased) => aliased,
        };
        let started = crate::time::now();
        let hal = {
            // Safety: the guard only lends this device's Vulkan handles; it is
            // dropped before the wgpu texture is made.
            let guard = unsafe { device.as_hal::<vulkan::Api>() }.ok_or(ImportError::NotVulkan)?;
            let hal_device: &vulkan::Device = &guard;
            let dev = hal_device.raw_device();
            let vk_image = unsafe {
                create_explicit_image(
                    dev,
                    aliased.format,
                    (aliased.width, aliased.height),
                    aliased.modifier,
                    (aliased.offset, aliased.stride),
                )
            }?;
            if let Err(e) = unsafe { dev.bind_image_memory(vk_image, aliased.memory.memory, 0) } {
                unsafe { dev.destroy_image(vk_image, None) };
                return Err(ImportError::Vulkan(format!("vkBindImageMemory: {e:?}")));
            }
            let dev_owned = dev.clone();
            let memory = Arc::clone(&aliased.memory);
            LIVE_IMAGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let drop_cb: vello::wgpu::hal::DropCallback = Box::new(move || {
                unsafe { dev_owned.destroy_image(vk_image, None) };
                LIVE_IMAGES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                drop(memory);
            });
            let desc = vello::wgpu::hal::TextureDescriptor {
                label: Some("dmabuf-client-frame"),
                size: wgpu::Extent3d {
                    width: aliased.width,
                    height: aliased.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: aliased.wgpu_format,
                usage: vello::wgpu::wgt::TextureUses::RESOURCE
                    | vello::wgpu::wgt::TextureUses::COPY_SRC,
                memory_flags: vello::wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            };
            unsafe {
                hal_device.texture_from_raw(
                    vk_image,
                    &desc,
                    Some(drop_cb),
                    vulkan::TextureMemory::ForeignExternal,
                )
            }
        };
        let desc = wgpu::TextureDescriptor {
            label: Some("dmabuf-client-frame"),
            size: wgpu::Extent3d {
                width: aliased.width,
                height: aliased.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: aliased.wgpu_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        // Safety: `hal` was made from this device a moment ago and not used.
        let texture = unsafe { device.create_texture_from_hal::<vulkan::Api>(hal, &desc) };
        WRAPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        WRAP_NANOS.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(texture)
    }
}

/// A `VkImage` of `format` whose layout in memory is the producer's: its
/// `modifier`, plane offset and row pitch, created for sampling.
///
/// # Safety
///
/// `dev` is a live device that enabled `VK_EXT_image_drm_format_modifier`.
unsafe fn create_explicit_image(
    dev: &ash::Device,
    format: vk::Format,
    (width, height): (u32, u32),
    modifier: u64,
    (offset, stride): (u32, u32),
) -> Result<vk::Image, ImportError> {
    let modifier = if modifier == DRM_FORMAT_MOD_INVALID {
        DRM_FORMAT_MOD_LINEAR
    } else {
        modifier
    };
    let plane_layouts = [vk::SubresourceLayout {
        offset: u64::from(offset),
        size: 0,
        row_pitch: u64::from(stride),
        array_pitch: 0,
        depth_pitch: 0,
    }];
    let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(modifier)
        .plane_layouts(&plane_layouts);
    let mut external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(SAMPLED_USAGE)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external)
        .push_next(&mut explicit);
    unsafe { dev.create_image(&info, None) }
        .map_err(|e| ImportError::Vulkan(format!("vkCreateImage: {e:?}")))
}

/// The Vulkan device a dma-buf is imported into, with the external-memory
/// extension loaded for it. Every handle derives from one live device.
struct VulkanImporter<'a> {
    hal_device: &'a vulkan::Device,
    ext_fd: ash::khr::external_memory_fd::Device,
    /// The device can import a plane with its producer's exact layout.
    explicit_layout: bool,
}

/// How an imported image will be used: Vulkan usage, the wgpu HAL uses that
/// describe it, and the debug label.
struct PlaneUsage {
    /// The buffer's DRM modifier, when the plane is imported with its exact
    /// layout (offset, row pitch) rather than as the driver's own linear one.
    modifier: Option<u64>,
    vk: vk::ImageUsageFlags,
    hal: vello::wgpu::wgt::TextureUses,
    label: &'static str,
}

impl<'a> VulkanImporter<'a> {
    fn new(hal_device: &'a vulkan::Device) -> Self {
        let ext_fd = ash::khr::external_memory_fd::Device::new(
            hal_device.shared_instance().raw_instance(),
            hal_device.raw_device(),
        );
        Self {
            hal_device,
            ext_fd,
            explicit_layout: has_modifier_extension(hal_device),
        }
    }

    /// The modifiers the driver lists for `format`, with their features.
    fn listed_modifiers(&self, format: vk::Format) -> Vec<vk::DrmFormatModifierPropertiesEXT> {
        let phys = self.hal_device.raw_physical_device();
        let instance = self.hal_device.shared_instance().raw_instance();
        // Two calls: the count, then the properties.
        let mut counted = vk::DrmFormatModifierPropertiesListEXT::default();
        {
            let mut query = vk::FormatProperties2::default().push_next(&mut counted);
            unsafe { instance.get_physical_device_format_properties2(phys, format, &mut query) };
        }
        let mut props = vec![
            vk::DrmFormatModifierPropertiesEXT::default();
            counted.drm_format_modifier_count as usize
        ];
        {
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
                .drm_format_modifier_properties(&mut props);
            let mut query = vk::FormatProperties2::default().push_next(&mut list);
            unsafe { instance.get_physical_device_format_properties2(phys, format, &mut query) };
        }
        props
    }

    /// Ask the driver whether it can sample `spec`'s format laid out with
    /// `modifier` as one plane, imported from a dma-buf, at this size.
    fn check_modifier(
        &self,
        spec: &PlaneSpec,
        modifier: u64,
        usage: vk::ImageUsageFlags,
    ) -> Result<(), ImportError> {
        let phys = self.hal_device.raw_physical_device();
        let instance = self.hal_device.shared_instance().raw_instance();
        let unsupported = ImportError::UnsupportedModifier { modifier };
        let props = self.listed_modifiers(spec.format);
        let Some(found) = props.iter().find(|p| p.drm_format_modifier == modifier) else {
            return Err(unsupported);
        };
        let wanted = vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_SRC;
        if found.drm_format_modifier_plane_count != 1
            || !found.drm_format_modifier_tiling_features.contains(wanted)
        {
            return Err(unsupported);
        }
        let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut chosen = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(modifier)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(spec.format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage)
            .push_next(&mut external)
            .push_next(&mut chosen);
        let mut external_props = vk::ExternalImageFormatProperties::default();
        let mut out = vk::ImageFormatProperties2::default().push_next(&mut external_props);
        unsafe { instance.get_physical_device_image_format_properties2(phys, &info, &mut out) }
            .map_err(|_| unsupported.clone())?;
        let extent = out.image_format_properties.max_extent;
        if extent.width < spec.width || extent.height < spec.height {
            return Err(unsupported);
        }
        if !external_props
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        {
            return Err(unsupported);
        }
        Ok(())
    }

    /// Import `plane`'s memory once, as the memory every image over the
    /// dma-buf is bound to ([`DmabufImage::texture`]).
    ///
    /// # Safety
    ///
    /// The device has `VK_EXT_image_drm_format_modifier` enabled.
    unsafe fn import_aliased(
        &self,
        plane: &PlaneRef<'_>,
        spec: &PlaneSpec,
        modifier: u64,
    ) -> Result<AliasedImage, ImportError> {
        let dev = self.hal_device.raw_device();
        let modifier = if modifier == DRM_FORMAT_MOD_INVALID {
            DRM_FORMAT_MOD_LINEAR
        } else {
            modifier
        };
        // The producer's pitch must at least hold a row.
        let row = u64::from(spec.width).saturating_mul(u64::from(spec.texel_bytes));
        if u64::from(plane.stride) < row {
            return Err(ImportError::UnsupportedModifier { modifier });
        }
        self.check_modifier(spec, modifier, SAMPLED_USAGE)?;

        // The memory requirements of the image, then no more use of it.
        let probe = unsafe {
            create_explicit_image(
                dev,
                spec.format,
                (spec.width, spec.height),
                modifier,
                (plane.offset, plane.stride),
            )
        }?;
        let reqs = unsafe { dev.get_image_memory_requirements(probe) };
        unsafe { dev.destroy_image(probe, None) };

        let dup = plane
            .fd
            .try_clone_to_owned()
            .map_err(|e| ImportError::Vulkan(format!("dup dmabuf fd: {e}")))?;
        // The GPU reads `reqs.size` bytes of this buffer: refuse one shorter.
        let have = dmabuf_len(&dup)?;
        if have < reqs.size {
            return Err(ImportError::Vulkan(format!(
                "dmabuf is {have} bytes but its image needs {} (offset {}, pitch {})",
                reqs.size, plane.offset, plane.stride
            )));
        }
        let fd_types = {
            let mut props = vk::MemoryFdPropertiesKHR::default();
            match unsafe {
                self.ext_fd.get_memory_fd_properties(
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    dup.as_raw_fd(),
                    &mut props,
                )
            } {
                Ok(()) => props.memory_type_bits,
                Err(_) => u32::MAX,
            }
        };
        let phys = self.hal_device.raw_physical_device();
        let instance = self.hal_device.shared_instance().raw_instance();
        let allowed = reqs.memory_type_bits & fd_types;
        let Some(mem_type) = find_memory_type(instance, phys, allowed) else {
            return Err(ImportError::Vulkan(format!(
                "no memory type accepts the imported dmabuf (image mask {:#x}, fd mask {fd_types:#x})",
                reqs.memory_type_bits
            )));
        };
        // Not a dedicated allocation: several images are bound to it over
        // time. With an explicit layout its size already covers the offset.
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(dup.as_raw_fd());
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(mem_type)
            .push_next(&mut import_info);
        let memory = unsafe { dev.allocate_memory(&alloc_info, None) }
            .map_err(|e| ImportError::Vulkan(format!("vkAllocateMemory: {e:?}")))?;
        // vkAllocateMemory consumed the fd; don't let `dup` close it.
        let _ = dup.into_raw_fd();
        Ok(AliasedImage {
            memory: Arc::new(AliasedMemory {
                dev: dev.clone(),
                memory,
            }),
            format: spec.format,
            wgpu_format: spec.wgpu_format,
            width: spec.width,
            height: spec.height,
            modifier,
            offset: plane.offset,
            stride: plane.stride,
        })
    }

    /// Import one plane as a `wgpu::Texture`.
    // Keeping acquisition and cleanup together makes the ownership transfer to
    // Vulkan auditable; splitting this sequence would obscure its early returns.
    unsafe fn import_plane(
        &self,
        plane: &PlaneRef<'_>,
        spec: &PlaneSpec,
        index: usize,
        modifier: u64,
    ) -> Result<<vulkan::Api as vello::wgpu::hal::Api>::Texture, ImportError> {
        unsafe {
            self.import_plane_with_usage(
                plane,
                spec,
                index,
                PlaneUsage {
                    modifier: Some(modifier),
                    vk: vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC,
                    hal: vello::wgpu::wgt::TextureUses::RESOURCE
                        | vello::wgpu::wgt::TextureUses::COPY_SRC,
                    label: "dmabuf-plane",
                },
            )
        }
    }

    /// Import one dma-buf plane with an explicit Vulkan/HAL use contract.
    ///
    /// # Safety
    ///
    /// `usage.hal` must describe `usage.vk`, because wgpu uses it for
    /// synchronization and layout transitions after taking the raw image.
    unsafe fn import_plane_with_usage(
        &self,
        plane: &PlaneRef<'_>,
        spec: &PlaneSpec,
        index: usize,
        usage: PlaneUsage,
    ) -> Result<<vulkan::Api as vello::wgpu::hal::Api>::Texture, ImportError> {
        let hal_device = self.hal_device;
        let dev = hal_device.raw_device();
        let phys = hal_device.raw_physical_device();
        let instance = hal_device.shared_instance().raw_instance();
        let ext_fd = &self.ext_fd;
        let PlaneUsage {
            modifier,
            vk: vk_usage,
            hal: hal_usage,
            label,
        } = usage;
        // Vulkan takes ownership of an imported fd on success, so hand it a
        // duplicate and leave the caller's original alone.
        let dup = plane
            .fd
            .try_clone_to_owned()
            .map_err(|e| ImportError::Vulkan(format!("dup dmabuf fd: {e}")))?;

        let mut ext_ci = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        // The exact layout the producer wrote, when the device can take it.
        // An unstated modifier is the linear layout, as before.
        let explicit = modifier.filter(|_| self.explicit_layout).map(|m| {
            if m == DRM_FORMAT_MOD_INVALID {
                DRM_FORMAT_MOD_LINEAR
            } else {
                m
            }
        });
        if let Some(modifier) = explicit {
            // The producer's pitch must at least hold a row, or the driver
            // would be told a layout that overlaps itself.
            let row = u64::from(spec.width).saturating_mul(u64::from(spec.texel_bytes));
            if u64::from(plane.stride) < row {
                return Err(ImportError::UnsupportedModifier { modifier });
            }
            self.check_modifier(spec, modifier, vk_usage)?;
        }
        let plane_layouts = [vk::SubresourceLayout {
            offset: u64::from(plane.offset),
            size: 0,
            row_pitch: u64::from(plane.stride),
            array_pitch: 0,
            depth_pitch: 0,
        }];
        let mut explicit_ci = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(explicit.unwrap_or(DRM_FORMAT_MOD_LINEAR))
            .plane_layouts(&plane_layouts);
        let mut image_ci = vk::ImageCreateInfo::default()
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
            // Without VK_EXT_image_drm_format_modifier, linear is the only
            // tiling that can be described.
            .tiling(if explicit.is_some() {
                vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
            } else {
                vk::ImageTiling::LINEAR
            })
            .usage(vk_usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut ext_ci);
        if explicit.is_some() {
            image_ci = image_ci.push_next(&mut explicit_ci);
        }

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
        let sub_layout = if explicit.is_none() {
            unsafe { dev.get_image_subresource_layout(vk_image, sub) }
        } else {
            // Not asked: with an explicit layout the driver's pitch is the producer's.
            vk::SubresourceLayout::default()
        };
        if explicit.is_none() && sub_layout.row_pitch != u64::from(plane.stride) {
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
        // The GPU reads `reqs.size` bytes (plus the bind offset) from this
        // buffer. A dma-buf shorter than that, which a producer's pitch or
        // offset can cause, would make the GPU read past it, so refuse before
        // the driver is asked to map it.
        let needed = reqs.size.saturating_add(if explicit.is_some() {
            0
        } else {
            u64::from(plane.offset)
        });
        let have = match dmabuf_len(&dup) {
            Ok(have) => have,
            Err(e) => {
                cleanup_image(dev);
                return Err(e);
            }
        };
        if have < needed {
            cleanup_image(dev);
            return Err(ImportError::Vulkan(format!(
                "dmabuf is {have} bytes but its image needs {needed} \
                 (offset {}, pitch {})",
                plane.offset, plane.stride
            )));
        }
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
        // With an explicit layout the offset is part of the image's own layout,
        // so the memory binds at zero and its size already covers the offset.
        let bind_offset = if explicit.is_some() {
            0
        } else {
            u64::from(plane.offset)
        };
        let use_dedicated = bind_offset == 0;
        // The allocation must cover the plane's offset as well as its size.
        let Some(size) = reqs.size.checked_add(bind_offset) else {
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

        if let Err(e) = unsafe { dev.bind_image_memory(vk_image, memory, bind_offset) } {
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
            label: Some(label),
            size: size3d,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: spec.wgpu_format,
            usage: hal_usage,
            memory_flags: vello::wgpu::hal::MemoryFlags::empty(),
            view_formats: vec![],
        };
        Ok(unsafe {
            hal_device.texture_from_raw(
                vk_image,
                &hal_desc,
                Some(drop_cb),
                // A producer outside this device wrote the image: its first
                // use acquires it from the foreign queue family.
                if explicit.is_some() {
                    vulkan::TextureMemory::ForeignExternal
                } else {
                    vulkan::TextureMemory::External
                },
            )
        })
    }
}

/// The bytes `fd`'s dma-buf holds (`media::dmabuf::dmabuf_size`): what the GPU
/// may read from it.
fn dmabuf_len(fd: &std::os::fd::OwnedFd) -> Result<u64, ImportError> {
    media::dmabuf::dmabuf_size(fd)
        .ok_or_else(|| ImportError::Vulkan("dmabuf size: the fd does not seek".into()))
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
    use std::os::fd::AsFd as _;

    fn one_plane(file: &std::fs::File, offset: u32, stride: u32) -> [PlaneRef<'_>; 1] {
        [PlaneRef {
            fd: file.as_fd(),
            offset,
            stride,
        }]
    }

    #[test]
    fn render_target_contract_accepts_only_exact_linear_scanout_formats() {
        // Case `a_tiled_modifier_is_refused_rather_than_sampled_as_pixels`.
        {
            // The AMD GFX9 tiled modifier a VA-API decoder exports. Importing it
            // as linear would read the tiling pattern as picture content.
            assert!(!is_linear(0x0200_0000_1040_1b04));
            assert!(is_linear(DRM_FORMAT_MOD_LINEAR));
            assert!(
                is_linear(DRM_FORMAT_MOD_INVALID),
                "unstated is treated as linear"
            );
        }

        // Case `scanout_target_contract_accepts_only_exact_linear_xrgb`.
        {
            let file = tempfile::tempfile().expect("temporary descriptor");
            let planes = one_plane(&file, 0, 256);
            let valid = DmabufRef {
                planes: &planes,
                fourcc: DRM_FORMAT_XRGB8888,
                modifier: DRM_FORMAT_MOD_LINEAR,
                width: 64,
                height: 32,
                colorimetry: Colorimetry::default(),
            };
            assert_eq!(
                validate_render_target(&valid),
                Ok((
                    vk::Format::B8G8R8A8_UNORM,
                    wgpu::TextureFormat::Bgra8Unorm,
                    4
                ))
            );

            let wrong_format = DmabufRef {
                fourcc: DRM_FORMAT_XBGR8888,
                ..valid
            };
            assert_eq!(
                validate_render_target(&wrong_format),
                Err(ImportError::UnsupportedFormat {
                    fourcc: DRM_FORMAT_XBGR8888
                })
            );

            let tiled = DmabufRef {
                modifier: 0x0200_0000_1040_1b04,
                ..valid
            };
            assert!(matches!(
                validate_render_target(&tiled),
                Err(ImportError::UnsupportedModifier { .. })
            ));
        }

        // Case `hdr_scanout_formats_map_to_their_ten_bit_and_half_float_views`.
        {
            let file = tempfile::tempfile().expect("temporary descriptor");
            let planes = one_plane(&file, 0, 256);
            let ten_bit = DmabufRef {
                planes: &planes,
                fourcc: DRM_FORMAT_XBGR2101010,
                modifier: DRM_FORMAT_MOD_LINEAR,
                width: 64,
                height: 32,
                colorimetry: Colorimetry::default(),
            };
            assert_eq!(
                validate_render_target(&ten_bit),
                Ok((
                    vk::Format::A2B10G10R10_UNORM_PACK32,
                    wgpu::TextureFormat::Rgb10a2Unorm,
                    4
                ))
            );
            // Half float needs eight bytes a pixel: 64 wide is 512 bytes.
            let half_float = DmabufRef {
                fourcc: DRM_FORMAT_ABGR16161616F,
                ..ten_bit
            };
            assert_eq!(
                validate_render_target(&half_float),
                Err(ImportError::StrideMismatch {
                    plane: 0,
                    producer: 256,
                    driver: 512,
                })
            );
            let wide_planes = one_plane(&file, 0, 512);
            let half_float = DmabufRef {
                planes: &wide_planes,
                fourcc: DRM_FORMAT_ABGR16161616F,
                ..ten_bit
            };
            assert_eq!(
                validate_render_target(&half_float),
                Ok((
                    vk::Format::R16G16B16A16_SFLOAT,
                    wgpu::TextureFormat::Rgba16Float,
                    8
                ))
            );
        }

        // Case `scanout_target_contract_rejects_offset_and_short_stride`.
        {
            let file = tempfile::tempfile().expect("temporary descriptor");
            let offset_planes = one_plane(&file, 4, 256);
            let offset = DmabufRef {
                planes: &offset_planes,
                fourcc: DRM_FORMAT_XRGB8888,
                modifier: DRM_FORMAT_MOD_LINEAR,
                width: 64,
                height: 32,
                colorimetry: Colorimetry::default(),
            };
            assert!(matches!(
                validate_render_target(&offset),
                Err(ImportError::Vulkan(message)) if message.contains("offset must be zero")
            ));

            let short_planes = one_plane(&file, 0, 64 * 4 - 1);
            let short = DmabufRef {
                planes: &short_planes,
                ..offset
            };
            assert_eq!(
                validate_render_target(&short),
                Err(ImportError::StrideMismatch {
                    plane: 0,
                    producer: 255,
                    driver: 256,
                })
            );
        }
    }

    #[test]
    fn plane_specs_map_each_fourcc_to_its_texture_layout_or_refuse_it() {
        // Case `nv12_maps_to_a_luma_and_a_half_resolution_chroma_plane`.
        {
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

        // Case `p010_uses_sixteen_bit_planes_for_its_ten_bit_samples`.
        {
            let (layout, specs) = plane_specs(DRM_FORMAT_P010, 3840, 2160).unwrap();
            assert_eq!(layout, PlaneLayout::BiPlanarYuv);
            assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::R16Unorm);
            assert_eq!(specs[1].wgpu_format, wgpu::TextureFormat::Rg16Unorm);
            assert_eq!(specs[0].texel_bytes, 2);
            assert_eq!(specs[1].texel_bytes, 4);
        }

        // Case `odd_dimensions_keep_a_whole_chroma_sample_for_the_last_row_and_column`.
        {
            // Rounding down would drop the right column and bottom row of chroma,
            // tinting the edges of an odd-sized picture.
            let (_, specs) = plane_specs(DRM_FORMAT_NV12, 1919, 1079).unwrap();
            assert_eq!((specs[1].width, specs[1].height), (960, 540));
        }

        // Case `yuyv_is_imported_at_half_width_because_one_texel_holds_two_pixels`.
        {
            let (layout, specs) = plane_specs(DRM_FORMAT_YUYV, 1920, 1080).unwrap();
            assert_eq!(layout, PlaneLayout::PackedYuv422);
            assert_eq!(specs.len(), 1);
            assert_eq!((specs[0].width, specs[0].height), (960, 1080));
            assert_eq!(specs[0].wgpu_format, wgpu::TextureFormat::Rgba8Unorm);
        }

        // Case `drm_rgb_names_map_to_the_matching_vulkan_byte_order`.
        {
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

            // Ten-bit and half-float client buffers keep their precision.
            let (_, ten) = plane_specs(DRM_FORMAT_XBGR2101010, 64, 64).unwrap();
            assert_eq!(ten[0].wgpu_format, wgpu::TextureFormat::Rgb10a2Unorm);
            assert_eq!(ten[0].format, vk::Format::A2B10G10R10_UNORM_PACK32);
            let (_, half) = plane_specs(DRM_FORMAT_ABGR16161616F, 64, 64).unwrap();
            assert_eq!(half[0].wgpu_format, wgpu::TextureFormat::Rgba16Float);
            assert_eq!(half[0].texel_bytes, 8);
        }

        // Case `an_unknown_fourcc_is_rejected_rather_than_guessed_at`.
        {
            assert!(plane_specs(media::dmabuf::fourcc(b'Z', b'Z', b'Z', b'Z'), 64, 64).is_none());
        }
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
            assert!(!s.contains('{'), "unformatted placeholder in {s:?}");
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
