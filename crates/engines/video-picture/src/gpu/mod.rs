//! GPU compositing for one frame: the layer stack, the grade, the effect chain,
//! the fade envelope and the transitions, in a wgpu compute pipeline.
//! [`crate::preview`] decides what the stack is; [`compositor::VideoCompositor`]
//! draws it.
//!
//! The compositor never decodes: it asks a [`FrameSource`] for a texture and
//! falls back to the clip's representative fill when there is none, which covers
//! a hermetic capture, the live preview and export, and tests alike.

pub mod compositor;
#[cfg(feature = "decode")]
pub mod media_source;
#[cfg(not(feature = "decode"))]
pub mod seek_source;
pub mod title;

use std::collections::HashMap;
use std::sync::Arc;

use vello::wgpu;

use video_edit::model::MediaId;

pub use compositor::{PendingFrame, VideoCompositor, upload_rgba};
#[cfg(feature = "decode")]
pub use media_source::{Fetch, MediaFrameSource};
#[cfg(not(feature = "decode"))]
pub use seek_source::{Fetch, SeekFrameSource as MediaFrameSource};

/// FNV-1a over each visual item's id and path, summed so the bin's order does not
/// count: a caller skips re-indexing when it has not changed.
pub fn media_signature(project: &video_edit::model::Project) -> u64 {
    let mut acc: u64 = 0;
    for item in &project.media {
        if !item.kind.is_visual() || item.kind.is_generated() {
            continue;
        }
        let mut hasher = fnv::FnvHasher::default();
        std::hash::Hasher::write(&mut hasher, item.id.as_bytes());
        std::hash::Hasher::write(&mut hasher, item.path.as_bytes());
        acc = acc.wrapping_add(std::hash::Hasher::finish(&hasher));
    }
    acc
}

#[derive(Clone)]
pub struct SourceTexture {
    pub texture: Arc<wgpu::Texture>,
    pub view: Arc<wgpu::TextureView>,
    pub width: u32,
    pub height: u32,
}

/// `None` is normal: the layer then draws its representative fill, so an
/// un-hydrated reference or a build with no decoder still renders.
pub trait FrameSource {
    /// A source that reuses textures across frames needs to know where a
    /// composite begins, so two layers never get the same reusable texture.
    fn begin_frame(&mut self) {}

    fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        source_time: f64,
    ) -> Option<SourceTexture>;
}

pub struct NoSources;

impl FrameSource for NoSources {
    fn resolve(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _media: MediaId,
        _source_time: f64,
    ) -> Option<SourceTexture> {
        None
    }
}

/// Each picture is uploaded once and cached by identity, so a paused playhead
/// re-uploads nothing. The closure returns `(width, height, rgba, identity)`; a
/// presentation timestamp is the natural identity.
pub struct CpuFrames<F> {
    produce: F,
    cache: HashMap<(MediaId, u64), SourceTexture>,
}

pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    pub identity: u64,
}

impl<F> CpuFrames<F>
where
    F: FnMut(MediaId, f64) -> Option<CpuFrame>,
{
    pub fn new(produce: F) -> Self {
        Self {
            produce,
            cache: HashMap::new(),
        }
    }
}

impl<F> FrameSource for CpuFrames<F>
where
    F: FnMut(MediaId, f64) -> Option<CpuFrame>,
{
    fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        source_time: f64,
    ) -> Option<SourceTexture> {
        let frame = (self.produce)(media, source_time)?;
        let key = (media, frame.identity);
        if let Some(hit) = self.cache.get(&key) {
            return Some(hit.clone());
        }
        let tex = upload_rgba(device, queue, frame.width, frame.height, &frame.rgba)?;
        // A handful of distinct pictures is all a frame needs; keep the map from
        // growing across a long export.
        if self.cache.len() > 16 {
            self.cache.clear();
        }
        self.cache.insert(key, tex.clone());
        Some(tex)
    }
}

/// Uses [`ui::offscreen::shared_gpu`] rather than an `Instance` of its own:
/// parallel `wgpu::Instance` creation races inside Mesa.
pub struct HeadlessCompositor {
    gpu: ui::offscreen::SharedGpu,
    compositor: VideoCompositor,
}

impl HeadlessCompositor {
    /// Fails rather than panicking on a box with no usable adapter, so callers
    /// can report it honestly.
    pub fn new() -> Result<Self, String> {
        let gpu = ui::offscreen::shared_gpu().map_err(|e| e.to_string())?;
        Ok(Self {
            gpu,
            compositor: VideoCompositor::new(),
        })
    }

    pub fn frame(
        &mut self,
        project: &video_edit::model::Project,
        playhead: f64,
        width: u32,
        height: u32,
        sources: &mut dyn FrameSource,
    ) -> Vec<u8> {
        self.compositor.render_rgba(
            &self.gpu.device,
            &self.gpu.queue,
            project,
            playhead,
            width,
            height,
            sources,
        )
    }

    /// [`Self::frame`] without waiting for the GPU, for a thread that may not
    /// (a browser page's).
    pub fn frame_later(
        &mut self,
        project: &video_edit::model::Project,
        playhead: f64,
        width: u32,
        height: u32,
        sources: &mut dyn FrameSource,
    ) -> PendingFrame {
        self.compositor.render_rgba_later(
            &self.gpu.device,
            &self.gpu.queue,
            project,
            playhead,
            width,
            height,
            sources,
        )
    }
}
