//! A retained target drawn on the CPU: the scene replayed by `tilo-cpu-raster`
//! into pixels the host holds, repainted only where the frame changed. The
//! native render worker draws on it for an adapter that rasterizes on the
//! CPU and uploads the pixels to its texture; a page whose browser offers no
//! WebGPU draws on it and presents the pixels to its canvas.
//!
//! The rasterizer itself is a seam: a host that links `tilo-cpu-raster`
//! (`cpu_raster`) draws with it directly, and one that ships it as code the
//! page fetches first ([`defer_cpu_raster`], `lazy-cpu-raster`) hands over a
//! [`MakeRaster`] once it is in. The canvas — its pixels, damage and scroll
//! copies — is here either way.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;

use vello::peniko::Color;
use vello_encoding::Encoding;

use crate::retained_raster::{DAMAGE_ONLY_REFUSED, FrameContent, bounded_present_damage};

/// Integer device-pixel rectangle: a damaged region of the target, and where
/// a scroll copy landed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn pixels(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }
}

/// One frame for [`Raster::paint`].
pub struct Paint<'a> {
    /// The frame's scene, already scaled into target pixels.
    pub encoding: &'a Encoding,
    /// The target's pixels: premultiplied RGBA8, rows of `stride` bytes.
    pub pixels: &'a mut [u8],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// What to repaint; every pixel when empty.
    pub regions: &'a [Region],
    /// What the frame has under it: the colour a region starts from.
    pub base_color: Color,
}

/// A CPU rasterizer: what [`CpuCanvas`] draws through.
pub trait Raster {
    fn paint(&mut self, paint: Paint<'_>);
}

/// A function that makes a rasterizer once its code is loaded.
pub type MakeRaster = fn() -> Box<dyn Raster>;

thread_local! {
    static RASTER: RefCell<RasterState> = RefCell::new(RasterState {
        make: None,
        fetch: None,
        loading: None,
    });
}

struct RasterState {
    /// The rasterizer's code, once the host holding it handed it over.
    make: Option<MakeRaster>,
    /// Where it fetches that code from (`lazy-cpu-raster`), and the load in
    /// flight.
    fetch: Option<fn() -> Pin<Box<dyn Future<Output = bool>>>>,
    loading: Option<Pin<Box<dyn Future<Output = bool>>>>,
}

/// Whether the rasterizer's code is in and a canvas can be made.
pub fn cpu_raster_loaded() -> bool {
    RASTER.with_borrow(|state| state.make.is_some())
}

/// The host hands over where to fetch the rasterizer's code from: the first
/// canvas asked for fetches it ([`Self::load_cpu_raster`]).
#[cfg(feature = "lazy-cpu-raster")]
pub fn defer_cpu_raster(fetch: fn() -> Pin<Box<dyn Future<Output = bool>>>) {
    RASTER.with_borrow_mut(|state| state.fetch = Some(fetch));
}

/// The rasterizer's code arrived: draw through `make` from now on.
pub fn install_cpu_raster(make: MakeRaster) {
    RASTER.with_borrow_mut(|state| state.make = Some(make));
}

/// Start fetching the rasterizer's code (once) and answer when it is in;
/// `false` when no host supplied a way to fetch it or it did not arrive.
#[allow(
    clippy::future_not_send,
    reason = "the page awaits this on its own thread, as every host without a raster to draw with does"
)]
pub async fn load_cpu_raster() -> bool {
    let load = RASTER.with_borrow_mut(|state| {
        if state.make.is_some() {
            return None;
        }
        if state.loading.is_none() {
            state.loading = state.fetch.map(|fetch| fetch());
        }
        state.loading.take()
    });
    match load {
        Some(load) => load.await,
        None => cpu_raster_loaded(),
    }
}

/// The rasterizer to draw with, once its code is in.
fn make_raster() -> Option<Box<dyn Raster>> {
    if let Some(make) = RASTER.with_borrow(|state| state.make) {
        return Some(make());
    }
    #[cfg(not(feature = "lazy-cpu-raster"))]
    {
        Some(Box::new(cpu_raster::Rasterizer::new()))
    }
    #[cfg(feature = "lazy-cpu-raster")]
    {
        None
    }
}

/// Worker threads a frame is rasterized on: `vello_cpu` spawns none on
/// `wasm32`, where it draws on the calling thread.
pub(crate) const THREADS: u16 = if cfg!(target_arch = "wasm32") { 0 } else { 4 };

pub struct CpuCanvas {
    rasterizer: Box<dyn Raster>,
    pixels: Vec<u8>,
    size: (u32, u32),
    valid: bool,
    regions: Vec<Region>,
    /// What the last frame changed and [`Self::upload`] sends: what it
    /// rasterized, and where its scroll copies landed.
    uploads: Vec<Region>,
    /// The frames' scenes hold only what their damage needs: one that must
    /// be rasterized whole is refused.
    damage_only: bool,
}

/// A canvas to rasterize frames into when they are rasterized on the CPU
/// (`on_cpu`), else none; also none when that host's rasterizer's code is
/// not in yet.
pub(crate) fn canvas(on_cpu: bool) -> Option<CpuCanvas> {
    on_cpu.then(CpuCanvas::new).flatten()
}

impl CpuCanvas {
    /// The canvas, or `None` when this host's rasterizer is not in
    /// ([`install_cpu_raster`]; the page fetches it first, `lazy-cpu-raster`).
    pub fn new() -> Option<Self> {
        make_raster().map(|rasterizer| Self {
            rasterizer,
            pixels: Vec::new(),
            size: (0, 0),
            valid: false,
            regions: Vec::new(),
            uploads: Vec::new(),
            damage_only: false,
        })
    }

    /// Whether the frames' scenes hold only what their damage needs
    /// (`RetainedRaster::rasterize_damage_only`).
    pub fn draw_damage_only(&mut self, damage_only: bool) {
        self.damage_only = damage_only;
    }

    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    pub fn rasterize(&mut self, frame: &FrameContent<'_>) -> Result<u64, String> {
        if frame.width == 0 || frame.height == 0 {
            return Err("CPU target dimensions must be nonzero".into());
        }
        let size = (frame.width, frame.height);
        let stride = frame.width.checked_mul(4).ok_or("CPU row overflow")?;
        let length = usize::try_from(u64::from(stride) * u64::from(frame.height))
            .map_err(|_| "CPU buffer overflow")?;
        if frame.width > u32::from(u16::MAX) || frame.height > u32::from(u16::MAX) {
            return Err("CPU target exceeds rasterizer dimensions".into());
        }
        if self.size != size {
            self.valid = false;
            if length > self.pixels.len() {
                self.pixels
                    .try_reserve(length.saturating_sub(self.pixels.len()))
                    .map_err(|error| error.to_string())?;
            }
            self.pixels.resize(length, 0);
            self.size = size;
        }
        // What a scroll keeps on screen moves before the damage around it
        // is rasterized; a copy that cannot be made repaints everything.
        let copied = !frame.full_redraw
            && self.valid
            && crate::scroll_copy::copy_pixels(
                &mut self.pixels,
                (frame.width, frame.height, stride as usize),
                frame.scroll_copies,
            );
        let whole =
            frame.full_redraw || !self.valid || (!copied && !frame.scroll_copies.is_empty());
        let bounded = bounded_present_damage(frame.width, frame.height, frame.damage_regions);
        if self.damage_only && (whole || bounded.is_none()) {
            // The copies may have moved the pixels: what is retained is no
            // longer the last frame.
            self.valid = false;
            return Err(DAMAGE_ONLY_REFUSED.to_owned());
        }
        self.regions = if whole {
            vec![Region::new(0, 0, frame.width, frame.height)]
        } else {
            bounded
                .map(|regions| {
                    regions
                        .into_iter()
                        .map(|r| Region::new(r.x, r.y, r.width, r.height))
                        .collect()
                })
                .unwrap_or_else(|| vec![Region::new(0, 0, frame.width, frame.height)])
        };
        self.rasterizer.paint(Paint {
            encoding: frame.scene.encoding(),
            pixels: &mut self.pixels,
            width: frame.width,
            height: frame.height,
            stride: stride as usize,
            regions: &self.regions,
            base_color: frame.base_color,
        });
        self.valid = true;
        self.uploads.clone_from(&self.regions);
        if !whole {
            self.uploads
                .extend(frame.scroll_copies.iter().filter_map(|copy| {
                    let (x, y) = copy.destination(frame.width, frame.height)?;
                    Some(Region::new(x, y, copy.width, copy.height))
                }));
        }
        Ok(self.regions.iter().map(Region::pixels).sum())
    }

    /// The canvas' extent in pixels, and where it has an extent at all.
    pub fn extent(&self) -> (u32, u32) {
        self.size
    }

    /// The canvas: premultiplied RGBA8, rows of `width * 4` bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The pixels [`Self::rasterize`] last drew, and where the scroll copies
    /// of that frame landed: what a host that presents the canvas itself
    /// sends, instead of [`Self::upload`]'s whole texture.
    pub fn drawn(&self) -> &[Region] {
        &self.uploads
    }

    pub fn upload(&self, queue: &vello::wgpu::Queue, texture: &vello::wgpu::Texture) {
        let stride = self.size.0.saturating_mul(4);
        for region in &self.uploads {
            let offset = u64::from(region.y) * u64::from(stride) + u64::from(region.x) * 4;
            let Some(bytes) = usize::try_from(offset)
                .ok()
                .and_then(|offset| self.pixels.get(offset..))
            else {
                continue;
            };
            queue.write_texture(
                vello::wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: vello::wgpu::Origin3d {
                        x: region.x,
                        y: region.y,
                        z: 0,
                    },
                    aspect: vello::wgpu::TextureAspect::All,
                },
                bytes,
                vello::wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(self.size.1),
                },
                vello::wgpu::Extent3d {
                    width: region.width,
                    height: region.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

#[cfg(not(feature = "lazy-cpu-raster"))]
mod cpu_raster {
    //! `tilo-cpu-raster`, drawn through [`super::Raster`].

    use tilo_cpu_raster::{CpuRasterizer, Options, Region as RasterRegion, Target};

    use super::{Paint, Raster};

    pub(super) struct Rasterizer {
        rasterizer: CpuRasterizer,
        regions: Vec<RasterRegion>,
    }

    impl Rasterizer {
        pub(super) fn new() -> Self {
            Self {
                rasterizer: CpuRasterizer::new(Options {
                    threads: super::THREADS,
                    ..Options::default()
                }),
                regions: Vec::new(),
            }
        }
    }

    impl Raster for Rasterizer {
        fn paint(&mut self, paint: Paint<'_>) {
            self.regions.clear();
            self.regions.extend(
                paint
                    .regions
                    .iter()
                    .map(|r| RasterRegion::new(r.x, r.y, r.width, r.height)),
            );
            let Ok(mut target) = Target::new(paint.pixels, paint.width, paint.height, paint.stride)
            else {
                return;
            };
            self.rasterizer.prepare(paint.encoding);
            self.rasterizer.render_prepared(
                &mut target,
                Some(&self.regions),
                tilo_cpu_raster::color::AlphaColor::new(paint.base_color.components),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::{
        Scene,
        kurbo::{Affine, Rect},
        peniko::{Color, Fill},
    };

    fn scene(rect: Rect) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &rect,
        );
        scene
    }

    fn render(canvas: &mut CpuCanvas, scene: &Scene, damage: &[Rect], full: bool) {
        canvas
            .rasterize(&FrameContent {
                scene,
                width: 16,
                height: 16,
                base_color: Color::TRANSPARENT,
                damage_regions: damage,
                scroll_copies: &[],
                full_redraw: full,
                backdrop: &crate::backdrop::BackdropFrame::default(),
            })
            .expect("valid CPU frame");
    }

    fn canvas() -> CpuCanvas {
        CpuCanvas::new().expect("a canvas to draw on")
    }

    #[test]
    fn damaged_move_erases_old_pixels_and_matches_full_render() {
        let mut canvas = canvas();
        render(
            &mut canvas,
            &scene(Rect::new(1.0, 1.0, 5.0, 5.0)),
            &[],
            true,
        );
        assert_eq!(&canvas.pixels[136..140], &[128, 0, 0, 128]);
        let next = scene(Rect::new(6.0, 1.0, 10.0, 5.0));
        render(&mut canvas, &next, &[Rect::new(1.0, 1.0, 10.0, 5.0)], false);
        let mut oracle = CpuCanvas::new().expect("a canvas to draw on");
        render(&mut oracle, &next, &[], true);
        assert_eq!(canvas.pixels, oracle.pixels);
        assert_eq!(canvas.regions, vec![Region::new(1, 1, 9, 4)]);
    }

    #[test]
    fn gpu_handoff_invalidates_cpu_damage_history() {
        let mut canvas = canvas();
        render(
            &mut canvas,
            &scene(Rect::new(1.0, 1.0, 5.0, 5.0)),
            &[],
            true,
        );
        canvas.invalidate();
        let next = scene(Rect::new(6.0, 1.0, 10.0, 5.0));
        render(&mut canvas, &next, &[Rect::new(6.0, 1.0, 10.0, 5.0)], false);
        let mut oracle = CpuCanvas::new().expect("a canvas to draw on");
        render(&mut oracle, &next, &[], true);
        assert_eq!(canvas.pixels, oracle.pixels);
        assert_eq!(canvas.regions, vec![Region::new(0, 0, 16, 16)]);
    }
}

/// The seam a host without `tilo-cpu-raster` linked draws through: the canvas
/// keeps the pixels and the damage, the host's own rasterizer paints them.
#[cfg(test)]
mod seam_tests {
    use super::*;

    thread_local! {
        /// Frames the installed rasterizer painted, and the regions each was
        /// asked for.
        static PAINTED: RefCell<Vec<Vec<Region>>> = const { RefCell::new(Vec::new()) };
    }

    struct Blot;
    impl Raster for Blot {
        fn paint(&mut self, paint: Paint<'_>) {
            // Opaque red over what it was asked for, so the test can see the
            // regions the canvas worked out arrive here.
            for region in paint.regions {
                for y in region.y..region.y.saturating_add(region.height) {
                    for x in region.x..region.x.saturating_add(region.width) {
                        let at = (y as usize) * paint.stride + (x as usize) * 4;
                        paint.pixels[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
                    }
                }
            }
            PAINTED.with_borrow_mut(|painted| painted.push(paint.regions.to_vec()));
        }
    }

    #[test]
    fn an_installed_rasterizer_paints_the_canvas_and_its_damage() {
        install_cpu_raster(|| Box::new(Blot));
        assert!(cpu_raster_loaded());
        PAINTED.with_borrow_mut(Vec::clear);
        let mut canvas = CpuCanvas::new().expect("the rasterizer is installed");
        let scene = vello::Scene::new();
        let backdrop = crate::backdrop::BackdropFrame::default();
        let whole = FrameContent {
            scene: &scene,
            width: 8,
            height: 4,
            base_color: Color::TRANSPARENT,
            damage_regions: &[],
            scroll_copies: &[],
            full_redraw: true,
            backdrop: &backdrop,
        };
        canvas.rasterize(&whole).expect("a full frame");
        let damaged = FrameContent {
            damage_regions: &[vello::kurbo::Rect::new(2.0, 1.0, 6.0, 3.0)],
            full_redraw: false,
            ..whole
        };
        canvas.rasterize(&damaged).expect("a damaged frame");
        let painted = PAINTED.with_borrow(Clone::clone);
        assert_eq!(
            painted,
            vec![vec![Region::new(0, 0, 8, 4)], vec![Region::new(2, 1, 4, 2)],],
            "the first frame is whole, the second only its damage"
        );
        // The pixels the rasterizer painted are the canvas the host presents.
        assert_eq!(&canvas.pixels()[..4], &[255, 0, 0, 255]);
        assert_eq!(canvas.drawn(), &[Region::new(2, 1, 4, 2)]);
    }
}
