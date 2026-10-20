//! GLES composition of the CPU renderer's retained canvases.
//!
//! Where the GPU cannot run vello (a Mali Bifrost under panvk), the CPU
//! rasterizes the desktop ([`super::cpu_present`]). Its GLES driver still
//! draws textured rectangles well: a 1080p frame of a few full-screen layers
//! takes a few milliseconds on a Mali-G52 MC1, where the four Cortex-A55
//! cores together need several times that to blend them. So the CPU keeps
//! each layer of the desktop in its own canvas ([`RetainedCanvas`]) and the
//! GPU composites the canvases and the pointer into scanout buffers: a strip
//! pan reads its canvas at another offset and rasterizes nothing.
//!
//! One unshared context draws everything, created without smithay's
//! per-texture fences: such a fence makes panfrost submit every draw as its
//! own batch, each reloading and writing back the whole framebuffer. KMS
//! waits for the composite through the scanout buffer's implicit fence.
//!
//! Canvases are uploaded where they changed. A canvas wider than the GPU's
//! largest texture (a strip's canvas reaches as far as its windows do) is
//! held in columns of textures.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "pixel rectangles on a screen and canvases whose sides fit i32 exactly"
)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::Modifier;
use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::{Capability, GlesFrame, GlesRenderer, GlesTexture, ffi};
use smithay::backend::renderer::{Bind, Color32F, Frame, ImportMem, Renderer, Texture};
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle, Size, Transform};
use tilo_cpu_raster::Region;
use vello::FineDamageRect;

use super::retained_canvas::RetainedCanvas;
use super::scanout::GbmScanoutSlot;
use crate::compositor::cursor::CursorBitmap;

/// Which retained canvas a layer is, so its textures outlive a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerKey {
    /// The whole desktop drawn as one canvas.
    Screen,
    Backdrop,
    Workspace,
    Strip(usize),
    Chrome,
}

/// Part of a canvas shown on screen: the canvas pixels of `src` land at
/// `dst` (the same size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub src: FineDamageRect,
    pub dst: (i32, i32),
}

/// One layer as a composite draws it, bottom to top.
pub struct LayerDraw<'a> {
    pub key: LayerKey,
    pub canvas: &'a RetainedCanvas,
    /// Canvas regions written since the layer was last composited.
    pub written: &'a [Region],
    pub placements: Vec<Placement>,
}

/// What one composite cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct ComposeStats {
    pub draws: usize,
    pub upload_px: u64,
    pub upload: Duration,
    /// CPU time issuing the composite, uploads included.
    pub total: Duration,
}

/// A canvas held as textures, in columns no wider than the GPU takes.
struct LayerTextures {
    generation: u64,
    size: (u32, u32),
    /// Each column's first canvas column and its texture.
    columns: Vec<(u32, GlesTexture)>,
}

/// The GLES compositor of one output.
pub struct GlCompositor {
    renderer: GlesRenderer,
    max_texture: u32,
    layers: HashMap<LayerKey, LayerTextures>,
    /// The pointer image, by the address of the bitmap it was made from.
    cursor: Option<(usize, GlesTexture)>,
}

impl std::fmt::Debug for GlCompositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlCompositor")
            .field("max_texture", &self.max_texture)
            .finish_non_exhaustive()
    }
}

fn rect(x: i32, y: i32, width: u32, height: u32) -> Rectangle<i32, Physical> {
    Rectangle::new((x, y).into(), (width as i32, height as i32).into())
}

impl GlCompositor {
    /// Open GLES on the render node `node`.
    ///
    /// # Errors
    ///
    /// Fails when no EGL device sits on `node`, or EGL or GLES cannot be
    /// brought up on it.
    pub fn new(node: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt as _;
        let want = std::fs::metadata(node)
            .map_err(|error| format!("stat {}: {error}", node.display()))?
            .rdev();
        let rdev = |path: std::path::PathBuf| std::fs::metadata(path).ok().map(|m| m.rdev());
        let device = EGLDevice::enumerate()
            .map_err(|error| format!("eglQueryDevicesEXT: {error}"))?
            .find(|device| {
                device.render_device_path().ok().and_then(rdev) == Some(want)
                    || device.drm_device_path().ok().and_then(rdev) == Some(want)
            })
            .ok_or_else(|| format!("no EGL device on {}", node.display()))?;
        // SAFETY: a live device just enumerated; the display lives in the
        // context, which the renderer owns.
        let display =
            unsafe { EGLDisplay::new(device) }.map_err(|error| format!("EGL display: {error}"))?;
        let context = EGLContext::new(&display).map_err(|error| format!("EGL context: {error}"))?;
        // SAFETY: the context is current only on this thread, which owns the
        // renderer (it is not `Send`).
        let capabilities = unsafe { GlesRenderer::supported_capabilities(&context) }
            .map_err(|error| format!("GLES capabilities: {error}"))?
            .into_iter()
            .filter(|capability| *capability != Capability::Fencing);
        // SAFETY: as above.
        let mut renderer = unsafe { GlesRenderer::with_capabilities(context, capabilities) }
            .map_err(|error| format!("GLES renderer: {error}"))?;
        let max_texture = renderer
            .with_context(|gl| {
                let mut max = 0;
                // SAFETY: one integer query on the current context.
                unsafe { gl.GetIntegerv(ffi::MAX_TEXTURE_SIZE, &raw mut max) };
                u32::try_from(max).unwrap_or(0)
            })
            .map_err(|error| format!("GLES query: {error}"))?;
        if max_texture < 1024 {
            return Err(format!("largest texture is {max_texture} pixels"));
        }
        Ok(Self {
            renderer,
            max_texture,
            layers: HashMap::new(),
            cursor: None,
        })
    }

    /// A scanout buffer as something this compositor renders into.
    ///
    /// # Errors
    ///
    /// Fails when the buffer cannot be exported.
    pub fn scanout_target(slot: &GbmScanoutSlot<'_>) -> Result<Dmabuf, String> {
        let fd = slot
            .fd
            .try_clone_to_owned()
            .map_err(|error| format!("dup scanout buffer: {error}"))?;
        let mut builder = Dmabuf::builder(
            (slot.width as i32, slot.height as i32),
            Fourcc::Xrgb8888,
            Modifier::from(slot.modifier),
            DmabufFlags::empty(),
        );
        builder.add_plane(fd, 0, slot.offset, slot.stride);
        builder
            .build()
            .ok_or_else(|| "scanout buffer has no plane".to_owned())
    }

    /// Bring `layer`'s textures up to date with its canvas.
    fn upload(&mut self, layer: &LayerDraw<'_>, stats: &mut ComposeStats) -> Result<(), String> {
        let canvas = layer.canvas;
        let (width, height) = canvas.size();
        let fresh = self.layers.get(&layer.key).is_none_or(|textures| {
            textures.generation != canvas.generation() || textures.size != (width, height)
        });
        let started = Instant::now();
        if fresh {
            let max = self.max_texture;
            let mut columns = Vec::new();
            let mut x0 = 0;
            while x0 < width {
                let column_width = max.min(width - x0);
                let texture = self.allocate_texture(column_width, height)?;
                columns.push((x0, texture));
                x0 += column_width;
            }
            self.layers.insert(
                layer.key,
                LayerTextures {
                    generation: canvas.generation(),
                    size: (width, height),
                    columns,
                },
            );
        }
        let whole = [Region::new(0, 0, width, height)];
        let regions = if fresh { &whole[..] } else { layer.written };
        let Some(textures) = self.layers.get(&layer.key) else {
            return Ok(());
        };
        let stride = canvas.stride();
        let pixels = canvas.pixels();
        let uploads: Vec<(u32, u32, Region)> = textures
            .columns
            .iter()
            .flat_map(|(x0, texture)| {
                let column_width = texture.width();
                regions.iter().filter_map(move |region| {
                    let left = region.x.max(*x0);
                    let right = (region.x + region.width).min(x0 + column_width);
                    (right > left).then(|| {
                        (
                            texture.tex_id(),
                            *x0,
                            Region::new(left, region.y, right - left, region.height),
                        )
                    })
                })
            })
            .collect();
        self.renderer
            .with_context(|gl| {
                for (texture, x0, region) in &uploads {
                    let first = region.y as usize * stride + region.x as usize * 4;
                    let Some(bytes) = pixels.get(first..) else {
                        continue;
                    };
                    // SAFETY: uploads from the canvas, whose rows are
                    // `stride` bytes; `bytes` holds every row the region
                    // reads, which the canvas size bounds.
                    unsafe {
                        gl.BindTexture(ffi::TEXTURE_2D, *texture);
                        gl.PixelStorei(ffi::UNPACK_ROW_LENGTH, (stride / 4) as i32);
                        gl.TexSubImage2D(
                            ffi::TEXTURE_2D,
                            0,
                            (region.x - x0) as i32,
                            region.y as i32,
                            region.width as i32,
                            region.height as i32,
                            ffi::RGBA,
                            ffi::UNSIGNED_BYTE,
                            bytes.as_ptr().cast(),
                        );
                        gl.PixelStorei(ffi::UNPACK_ROW_LENGTH, 0);
                        gl.BindTexture(ffi::TEXTURE_2D, 0);
                    }
                }
            })
            .map_err(|error| format!("layer upload: {error}"))?;
        stats.upload_px += uploads
            .iter()
            .map(|(_, _, region)| u64::from(region.width) * u64::from(region.height))
            .sum::<u64>();
        stats.upload += started.elapsed();
        Ok(())
    }

    fn allocate_texture(&mut self, width: u32, height: u32) -> Result<GlesTexture, String> {
        let tex = self
            .renderer
            .with_context(|gl| {
                let mut tex = 0;
                // SAFETY: plain texture calls on the current context; the
                // storage is left undefined until the first upload.
                unsafe {
                    gl.GenTextures(1, &raw mut tex);
                    gl.BindTexture(ffi::TEXTURE_2D, tex);
                    for (parameter, value) in [
                        (ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE),
                        (ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE),
                        (ffi::TEXTURE_MIN_FILTER, ffi::NEAREST),
                        (ffi::TEXTURE_MAG_FILTER, ffi::NEAREST),
                    ] {
                        gl.TexParameteri(ffi::TEXTURE_2D, parameter, value as i32);
                    }
                    gl.TexImage2D(
                        ffi::TEXTURE_2D,
                        0,
                        ffi::RGBA8 as i32,
                        width as i32,
                        height as i32,
                        0,
                        ffi::RGBA,
                        ffi::UNSIGNED_BYTE,
                        std::ptr::null(),
                    );
                    gl.BindTexture(ffi::TEXTURE_2D, 0);
                }
                tex
            })
            .map_err(|error| format!("layer texture: {error}"))?;
        let size: Size<i32, BufferCoord> = (width as i32, height as i32).into();
        // SAFETY: `tex` was made just now on this renderer's context, which
        // owns it from here and deletes it with the texture.
        Ok(unsafe { GlesTexture::from_raw(&self.renderer, Some(ffi::RGBA8), false, tex, size) })
    }

    fn cursor_texture(&mut self, bitmap: &Arc<CursorBitmap>) -> Result<GlesTexture, String> {
        let key = Arc::as_ptr(bitmap) as usize;
        if let Some((cached, texture)) = &self.cursor
            && *cached == key
        {
            return Ok(texture.clone());
        }
        let texture = self
            .renderer
            .import_memory(
                bitmap.rgba(),
                Fourcc::Abgr8888,
                (bitmap.width as i32, bitmap.height as i32).into(),
                false,
            )
            .map_err(|error| format!("cursor upload: {error}"))?;
        self.cursor = Some((key, texture.clone()));
        Ok(texture)
    }

    /// Composite `layers` and the pointer (its image and top-left corner)
    /// into `target`, a scanout buffer of `size`.
    ///
    /// # Errors
    ///
    /// Fails when an upload, the bind or a draw fails; the target then
    /// holds nothing to show.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "`finish` consumes the frame; only an error returns before it, dropping the frame there"
    )]
    pub fn compose(
        &mut self,
        target: &mut Dmabuf,
        size: (u32, u32),
        layers: &[LayerDraw<'_>],
        cursor: Option<(&Arc<CursorBitmap>, i32, i32)>,
    ) -> Result<ComposeStats, String> {
        let started = Instant::now();
        let mut stats = ComposeStats::default();
        for layer in layers {
            self.upload(layer, &mut stats)?;
        }
        let pointer = match cursor {
            Some((bitmap, x, y)) => Some((self.cursor_texture(bitmap)?, bitmap, x, y)),
            None => None,
        };
        let Self {
            renderer,
            layers: textures,
            ..
        } = self;
        let mut bound = renderer
            .bind(target)
            .map_err(|error| format!("bind scanout buffer: {error}"))?;
        let mut frame = renderer
            .render(
                &mut bound,
                (size.0 as i32, size.1 as i32).into(),
                Transform::Normal,
            )
            .map_err(|error| format!("begin frame: {error}"))?;
        // Clearing first tells a tiling GPU not to load the buffer's old
        // contents into its tiles: a full extra read of the frame.
        frame
            .clear(Color32F::BLACK, &[rect(0, 0, size.0, size.1)])
            .map_err(|error| format!("clear frame: {error}"))?;
        for layer in layers {
            let Some(columns) = textures.get(&layer.key) else {
                continue;
            };
            for placement in &layer.placements {
                let inked = layer.canvas.inked_parts(placement.src);
                stats.draws += draw_placement(&mut frame, &columns.columns, *placement, &inked)?;
            }
        }
        if let Some((texture, bitmap, x, y)) = pointer {
            let dst = rect(x, y, bitmap.width, bitmap.height);
            let src = Rectangle::<f64, BufferCoord>::from_size(
                (f64::from(bitmap.width), f64::from(bitmap.height)).into(),
            );
            frame
                .render_texture_from_to(
                    &texture,
                    src,
                    dst,
                    &[Rectangle::from_size(dst.size)],
                    &[],
                    Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
                .map_err(|error| format!("draw pointer: {error}"))?;
            stats.draws += 1;
        }
        let _sync = frame
            .finish()
            .map_err(|error| format!("finish frame: {error}"))?;
        drop(bound);
        stats.total = started.elapsed();
        Ok(stats)
    }
}

/// Draw the `inked` parts (canvas pixels) of `placement` from a canvas held
/// in `columns`: a tiling GPU then runs only the tiles that show something.
/// Returns the draws.
fn draw_placement(
    frame: &mut GlesFrame<'_, '_>,
    columns: &[(u32, GlesTexture)],
    placement: Placement,
    inked: &[FineDamageRect],
) -> Result<usize, String> {
    let mut draws = 0;
    let src = placement.src;
    for (x0, texture) in columns {
        let left = src.x.max(*x0);
        let right = (src.x + src.width).min(x0 + texture.width());
        if right <= left || src.height == 0 {
            continue;
        }
        let width = right - left;
        let dst = rect(
            placement.dst.0 + (left - src.x) as i32,
            placement.dst.1,
            width,
            src.height,
        );
        let from = Rectangle::<f64, BufferCoord>::new(
            (f64::from(left - x0), f64::from(src.y)).into(),
            (f64::from(width), f64::from(src.height)).into(),
        );
        // Each inked part within this column, relative to `dst`.
        let parts: Vec<Rectangle<i32, Physical>> = inked
            .iter()
            .filter_map(|part| {
                let x0 = part.x.max(left);
                let x1 = (part.x + part.width).min(right);
                (x1 > x0).then(|| {
                    rect(
                        (x0 - left) as i32,
                        (part.y - src.y) as i32,
                        x1 - x0,
                        part.height,
                    )
                })
            })
            .collect();
        if parts.is_empty() {
            continue;
        }
        frame
            .render_texture_from_to(
                texture,
                from,
                dst,
                &parts,
                &[],
                Transform::Normal,
                1.0,
                None,
                &[],
            )
            .map_err(|error| format!("draw layer: {error}"))?;
        draws += 1;
    }
    Ok(draws)
}
