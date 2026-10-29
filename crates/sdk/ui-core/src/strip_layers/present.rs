//! Presenting frames drawn as layers: the layer textures a host keeps, the
//! work each frame takes on them, and the composite into the host's output.
//!
//! The host supplies the raster ([`LayerRaster`]): how a scene is rendered
//! into one layer texture, under which completion policy, and how the
//! composite reaches its output. The compositor renders under a frame
//! deadline and waits for the GPU; an app's render worker submits and
//! presents. What a frame rasterises, copies and composites is decided here,
//! the same for both.

use vello::peniko::Color;
use vello::{FineDamageRect, Scene, wgpu};

use super::{
    BandCopy, FrameCache, Layer, LayerTexture, LayerWork, Shown, StripLayers, WorkspaceWork,
    append_panned, plan, warm_bands,
};
use crate::damage::area;

/// What a [`LayerPresenter`] rasterises with and composites through.
pub trait LayerRaster {
    type Error;

    fn device(&self) -> &wgpu::Device;

    fn queue(&self) -> &wgpu::Queue;

    /// The output's size, which every layer texture has too.
    fn size(&self) -> (u32, u32);

    /// Whether [`Self::render`] can rasterise only some regions of a layer.
    fn sparse(&self) -> bool;

    /// What the backdrop layer is rasterised over.
    fn base_color(&self) -> Color;

    /// Rasterise `scene` over `base` into `layer`: within `regions` only,
    /// when given, keeping the rest of the texture.
    ///
    /// # Errors
    /// When the render fails or misses the host's completion policy.
    fn render(
        &mut self,
        layer: &mut LayerTexture,
        scene: &Scene,
        regions: Option<Vec<FineDamageRect>>,
        base: Color,
    ) -> Result<(), Self::Error>;

    /// Encode with `encode` what writes the output (the composite, writing
    /// the output view it is given), and submit it under the host's
    /// completion policy.
    ///
    /// # Errors
    /// When the submission fails or misses the host's completion policy.
    fn composite(
        &mut self,
        encode: impl FnOnce(&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(), Self::Error>;
}

/// The layer textures of one output and what they hold.
#[derive(Default)]
pub struct LayerPresenter {
    /// Allocated by the first frame that comes as layers.
    layers: Option<StripLayers>,
    /// The work the latest presented frame planned.
    last_work: Option<LayerWork>,
}

impl LayerPresenter {
    /// Whether `cache` can be presented on an output of `size`: it has
    /// strips, one drawing for each, on a canvas at least as wide.
    #[must_use]
    pub fn can_present(cache: &FrameCache, size: (u32, u32)) -> bool {
        !cache.strips.is_empty()
            && cache.strips.len() == cache.layers.strips.len()
            && cache.width >= size.0
    }

    /// Composite `cache`, frame `revision`, into the output: a strip pan
    /// copies what stays on screen and rasterises only what it uncovered.
    /// `holds` is the frame the output target holds now, when it holds a
    /// complete one; only what changed since is composited when that is the
    /// frame last composited from the layers.
    ///
    /// # Errors
    /// As the raster's; a failure forgets what the layers hold, so the next
    /// frame rasterises them whole.
    pub fn present<R: LayerRaster>(
        &mut self,
        raster: &mut R,
        cache: &FrameCache,
        revision: u64,
        holds: Option<u64>,
    ) -> Result<(), R::Error> {
        let size = raster.size();
        let layers = self.layers_for(raster.device(), size);
        let work = match layers.shown.as_ref() {
            Some(shown) => {
                let current = shown.revision.is_some() && shown.revision == holds;
                plan(Some(shown), cache, size, current)
            }
            None => plan(None, cache, size, false),
        };
        let result = run(layers, raster, cache, &work);
        layers.shown = result.is_ok().then(|| Shown::of(cache, revision));
        self.last_work = Some(work);
        result
    }

    /// Bring the layer textures up to `cache` without compositing it: the
    /// content at rest drawn as layers, so that the first frame of the next
    /// pan copies and composites instead of rasterising every layer.
    /// Rasterises only the layers that differ from what the textures hold, a
    /// whole one in bands ([`warm_bands`]), and stops between two renders as
    /// soon as `keep_going` says a frame waits, keeping what it finished.
    /// The output is left alone. Returns the pixels rasterised.
    ///
    /// # Errors
    /// As the raster's; a failure forgets what the layers hold.
    pub fn warm<R: LayerRaster>(
        &mut self,
        raster: &mut R,
        cache: &FrameCache,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<u64, R::Error> {
        let size = raster.size();
        let layers = self.layers_for(raster.device(), size);
        layers.prime_composite(raster.device(), raster.queue());
        let work = plan(layers.shown.as_ref(), cache, size, false);
        if work.rasterises_nothing() {
            return Ok(0);
        }
        let mut shown = layers.shown.take().unwrap_or_else(Shown::unknown);
        // The output keeps the frame it shows, which these textures no
        // longer composite to.
        shown.revision = None;
        let warmed = warm_into(layers, raster, &mut shown, cache, &work, keep_going);
        layers.shown = warmed.is_ok().then_some(shown);
        warmed
    }

    /// The output went from `previous` to `revision` without a pixel
    /// changing: layers it was composited from as `previous` are still what
    /// it shows, so the next pan composites only what moves.
    pub fn kept(&mut self, previous: u64, revision: u64) {
        if let Some(shown) = self
            .layers
            .as_mut()
            .and_then(|layers| layers.shown.as_mut())
            && shown.revision == Some(previous)
        {
            shown.revision = Some(revision);
        }
    }

    /// What the layer textures hold is no longer known (the colour the
    /// backdrop is rasterised over changed, or the renderer lost geometry):
    /// the next frame rasterises every layer.
    pub fn forget(&mut self) {
        if let Some(layers) = self.layers.as_mut() {
            layers.shown = None;
        }
    }

    /// Whether the layer textures hold every layer of some frame: then a
    /// frame drawn as layers composites from them, rasterising only what
    /// changed.
    #[must_use]
    pub fn holds_layers(&self) -> bool {
        self.layers
            .as_ref()
            .and_then(|layers| layers.shown.as_ref())
            .is_some_and(Shown::complete)
    }

    /// The work the latest [`Self::present`] planned.
    #[must_use]
    pub fn last_work(&self) -> Option<&LayerWork> {
        self.last_work.as_ref()
    }

    /// The pixels the latest [`Self::present`] rasterised and composited,
    /// on an output of `size`, for counters.
    #[must_use]
    pub fn last_pixels(&self, size: (u32, u32)) -> Option<(u64, u64)> {
        self.last_work
            .as_ref()
            .map(|work| (work.raster_pixels(size), work.composite_pixels()))
    }

    fn layers_for(&mut self, device: &wgpu::Device, size: (u32, u32)) -> &mut StripLayers {
        if self
            .layers
            .as_ref()
            .is_none_or(|layers| layers.size != size)
        {
            self.layers = None;
        }
        self.layers
            .get_or_insert_with(|| StripLayers::new(device, size))
    }
}

/// [`LayerPresenter::present`]'s renders and composite.
fn run<R: LayerRaster>(
    layers: &mut StripLayers,
    raster: &mut R,
    cache: &FrameCache,
    work: &LayerWork,
) -> Result<(), R::Error> {
    let transparent = Color::TRANSPARENT;
    if work.backdrop {
        let base = raster.base_color();
        let texture = layers.texture_mut(Layer::Backdrop);
        raster.render(texture, &cache.layers.backdrop, None, base)?;
    }
    // The chrome's changed tiles, or all of it.
    let chrome_tiles = work.chrome_tiles.clone().filter(|_| raster.sparse());
    if work.chrome && chrome_tiles.as_ref().is_none_or(|tiles| !tiles.is_empty()) {
        let texture = layers.texture_mut(Layer::Chrome);
        raster.render(texture, &cache.layers.chrome, chrome_tiles, transparent)?;
    }
    match &work.workspace {
        WorkspaceWork::Unchanged => {}
        WorkspaceWork::Whole => {
            let scene = panned(cache);
            raster.render(
                layers.texture_mut(Layer::Workspace),
                &scene,
                None,
                transparent,
            )?;
        }
        WorkspaceWork::Pan { copies, uncovered } => {
            let copied = copy_bands(layers, raster, copies, "tilo-strip-layer-pan");
            if !copied || !uncovered.is_empty() {
                let scene = panned(cache);
                let regions = (copied && raster.sparse()).then(|| uncovered.clone());
                raster.render(
                    layers.texture_mut(Layer::Workspace),
                    &scene,
                    regions,
                    transparent,
                )?;
            }
        }
    }
    let layers = &*layers;
    raster.composite(|device, queue, encoder, output| {
        if let Some(rect) = work.composite {
            layers.encode_composite(
                super::CompositeTarget {
                    device,
                    queue,
                    encoder,
                    output,
                },
                rect,
            );
        }
    })
}

/// The strips layer's scene as the screen shows it.
fn panned(cache: &FrameCache) -> Scene {
    let mut scene = Scene::new();
    append_panned(&mut scene, &cache.layers, &cache.strips);
    scene
}

/// Move the strips layer's bands `copies` names. `false` when they were not
/// moved: the layer is then rasterised whole.
fn copy_bands<R: LayerRaster>(
    layers: &mut StripLayers,
    raster: &R,
    copies: &[BandCopy],
    label: &'static str,
) -> bool {
    if copies.is_empty() {
        return true;
    }
    let mut encoder = raster
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    let moved = layers.encode_copies(raster.device(), &mut encoder, copies);
    raster.queue().submit([encoder.finish()]);
    moved
}

fn area_of(rects: &[FineDamageRect]) -> u64 {
    rects.iter().copied().map(area).fold(0, u64::saturating_add)
}

/// [`LayerPresenter::warm`]'s renders, noting in `shown` each layer as it
/// is done. Returns the pixels rasterised.
fn warm_into<R: LayerRaster>(
    layers: &mut StripLayers,
    raster: &mut R,
    shown: &mut Shown,
    cache: &FrameCache,
    work: &LayerWork,
    keep_going: &dyn Fn() -> bool,
) -> Result<u64, R::Error> {
    let size = raster.size();
    let bands = warm_bands(size).filter(|_| raster.sparse());
    let whole = u64::from(size.0).saturating_mul(u64::from(size.1));
    let mut pixels = 0_u64;
    for (layer, wanted) in [
        (Layer::Backdrop, work.backdrop),
        (Layer::Chrome, work.chrome),
    ] {
        if !wanted {
            continue;
        }
        if !keep_going() {
            return Ok(pixels);
        }
        // A chrome texture that holds the drawing the chrome changed from
        // takes only the tiles that changed (a clock tick).
        let damaged = work
            .chrome_tiles
            .clone()
            .filter(|_| layer == Layer::Chrome && raster.sparse());
        shown.forget(layer);
        let (scene, base) = if layer == Layer::Backdrop {
            (&cache.layers.backdrop, raster.base_color())
        } else {
            (&cache.layers.chrome, Color::TRANSPARENT)
        };
        if let Some(rects) = damaged {
            let rasterised = area_of(&rects);
            if !rects.is_empty() {
                raster.render(layers.texture_mut(layer), scene, Some(rects), base)?;
            }
            shown.record(layer, cache);
            pixels = pixels.saturating_add(rasterised);
            continue;
        }
        let texture = layers.texture_mut(layer);
        if !warm_whole(raster, texture, (scene, base), bands.as_deref(), keep_going)? {
            return Ok(pixels);
        }
        shown.record(layer, cache);
        pixels = pixels.saturating_add(whole);
    }
    if work.workspace == WorkspaceWork::Unchanged || !keep_going() {
        return Ok(pixels);
    }
    shown.forget(Layer::Workspace);
    let scene = panned(cache);
    let strips = match &work.workspace {
        WorkspaceWork::Unchanged => Some(0),
        WorkspaceWork::Whole => {
            let texture = layers.texture_mut(Layer::Workspace);
            let done = warm_whole(
                raster,
                texture,
                (&scene, Color::TRANSPARENT),
                bands.as_deref(),
                keep_going,
            )?;
            done.then_some(whole)
        }
        WorkspaceWork::Pan { copies, uncovered } => {
            let copied = copy_bands(layers, raster, copies, "tilo-strip-layer-warm-pan");
            if !copied || !uncovered.is_empty() {
                let regions = (copied && raster.sparse()).then(|| uncovered.clone());
                raster.render(
                    layers.texture_mut(Layer::Workspace),
                    &scene,
                    regions,
                    Color::TRANSPARENT,
                )?;
            }
            Some(area_of(uncovered))
        }
    };
    if let Some(strips) = strips {
        shown.record(Layer::Workspace, cache);
        pixels = pixels.saturating_add(strips);
    }
    Ok(pixels)
}

/// Rasterise `scene` over `base` into all of `texture`, band by band when
/// `bands` cuts it, asking `keep_going` between two bands. Whether the
/// whole layer got drawn.
fn warm_whole<R: LayerRaster>(
    raster: &mut R,
    texture: &mut LayerTexture,
    (scene, base): (&Scene, Color),
    bands: Option<&[FineDamageRect]>,
    keep_going: &dyn Fn() -> bool,
) -> Result<bool, R::Error> {
    let Some(bands) = bands else {
        raster.render(texture, scene, None, base)?;
        return Ok(true);
    };
    for (index, band) in bands.iter().enumerate() {
        if index > 0 && !keep_going() {
            return Ok(false);
        }
        raster.render(texture, scene, Some(vec![*band]), base)?;
    }
    Ok(true)
}

/// The raster an app's render worker presents layers with: vello renders
/// each layer straight into its texture and the composite writes the
/// worker's retained target, all submitted in order on one queue, so
/// nothing waits for the GPU here; presenting the target follows them.
pub struct QueuedRaster<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut vello::Renderer,
    /// The retained target the composite writes.
    pub output: &'a wgpu::TextureView,
    pub size: (u32, u32),
    pub base_color: Color,
}

impl LayerRaster for QueuedRaster<'_> {
    type Error = vello::Error;

    fn device(&self) -> &wgpu::Device {
        self.device
    }

    fn queue(&self) -> &wgpu::Queue {
        self.queue
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn sparse(&self) -> bool {
        sparse_supported(self.renderer)
    }

    fn base_color(&self) -> Color {
        self.base_color
    }

    fn render(
        &mut self,
        layer: &mut LayerTexture,
        scene: &Scene,
        regions: Option<Vec<FineDamageRect>>,
        base: Color,
    ) -> Result<(), vello::Error> {
        let params = vello::RenderParams {
            base_color: base,
            width: layer.size.0,
            height: layer.size.1,
            antialiasing_method: vello::AaConfig::Area,
        };
        if let Some(regions) = regions.filter(|_| sparse_supported(self.renderer))
            && render_regions(self, layer, scene, &params, &regions)?
        {
            return Ok(());
        }
        self.renderer
            .render_to_texture(self.device, self.queue, scene, &layer.view, &params)
    }

    fn composite(
        &mut self,
        encode: impl FnOnce(&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(), vello::Error> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tilo-strip-layer-composite"),
            });
        encode(self.device, self.queue, &mut encoder, self.output);
        self.queue.submit([encoder.finish()]);
        Ok(())
    }
}

/// Vello's sparse fine pass, which only the native build carries.
#[cfg(feature = "native")]
fn sparse_supported(renderer: &vello::Renderer) -> bool {
    renderer.sparse_fine_supported()
}

#[cfg(not(feature = "native"))]
fn sparse_supported(_: &vello::Renderer) -> bool {
    false
}

/// Render only `regions` of `layer`; `false`, with nothing rendered, when
/// the renderer cannot.
#[cfg(feature = "native")]
fn render_regions(
    raster: &mut QueuedRaster<'_>,
    layer: &LayerTexture,
    scene: &Scene,
    params: &vello::RenderParams,
    regions: &[FineDamageRect],
) -> Result<bool, vello::Error> {
    Ok(raster
        .renderer
        .render_to_texture_with_damage(
            raster.device,
            raster.queue,
            scene,
            &layer.view,
            params,
            regions,
        )?
        .is_some())
}

#[cfg(not(feature = "native"))]
fn render_regions(
    _: &mut QueuedRaster<'_>,
    _: &LayerTexture,
    _: &Scene,
    _: &vello::RenderParams,
    _: &[FineDamageRect],
) -> Result<bool, vello::Error> {
    Ok(false)
}
