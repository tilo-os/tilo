//! Decoded pictures for the compositor, straight from the media engine.
//!
//! This is the [`FrameSource`] the real editor uses — the preview canvas and
//! the export both pull layers through it, so both see the same picture for
//! the same instant.
//!
//! ## Why planar is the path that matters
//!
//! A hardware decoder does not produce RGBA. `nvh264dec` emits NV12 in system
//! memory, and converting that to RGBA on the CPU costs a full-frame pass over
//! two million pixels and turns 3.1 MB of upload into 8.3 MB. So the planar
//! payload goes to the GPU *as planes* ([`upload_planar`]) and the YUV
//! arithmetic happens in a shader ([`YuvConverter`]). No colour conversion
//! runs on the CPU at all.
//!
//! The other two payloads are real but secondary here:
//!
//!   * `Rgba` — the software fallback (`avdec_h264`), uploaded as-is.
//!   * `Dmabuf` — a picture still in GPU memory. It cannot be imported on this
//!     stack: wgpu-hal does not enable `VK_EXT_image_drm_format_modifier`, so
//!     only a linear modifier could ever import and VA-API hands out tiled
//!     buffers, while NVDEC exports CUDA memory rather than a dmabuf at all.
//!     Rather than drop those frames, a source that yields one is moved to a
//!     software session, which lands its pictures in system memory where they
//!     can be painted.
//!
//! [`upload_planar`]: ui::gpu_video::upload_planar
//! [`YuvConverter`]: ui::gpu_video::YuvConverter

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use media::{EngineConfig, Frame, FramePayload, SessionPool};
use ui::gpu_video::{YuvConverter, upload_planar};
use vello::wgpu;

use crate::model::{MediaId, Project};

use super::{FrameSource, SourceTexture, compositor::upload_rgba};

/// How a caller wants frames fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch {
    /// Block until the frame at the requested instant is decoded. What an
    /// export wants: every frame must be the right one, and there is no
    /// interactive deadline to miss.
    Exact,
    /// Never block. Serve the cached frame when there is one, otherwise ask
    /// the pipeline to move and paint the last picture this source produced
    /// until it arrives. What a preview wants: a scrub must not stall the UI
    /// thread, and a frame or two of lag beats a frozen window.
    Responsive,
}

/// The picture most recently painted for a source, held so a `Responsive`
/// fetch has something to show while the pipeline seeks.
#[derive(Clone)]
struct LastGood {
    pts_ms: u64,
    texture: SourceTexture,
}

/// A [`FrameSource`] backed by the shared media engine.
pub struct MediaFrameSource {
    pool: SessionPool,
    /// Sessions for sources whose frames cannot be painted GPU-resident.
    software: SessionPool,
    demoted: HashSet<MediaId>,
    /// Decodable media in the project: where the bytes are, how fast the
    /// source runs (so the frame cache buckets line up with real frames), and
    /// the instant of its last frame in milliseconds when its length is known.
    sources: HashMap<MediaId, (media::Source, f64, Option<u64>)>,
    /// One converter per layer slot. Every converter owns its output texture
    /// and reuses it across frames, so two layers converting in the same
    /// composite must not share one — otherwise the second would overwrite
    /// the first and both layers would paint the same picture.
    converters: Vec<YuvConverter>,
    slot: usize,
    last: HashMap<MediaId, LastGood>,
    /// The instant each source was last asked for, so a forward step can be
    /// told from a jump.
    served: HashMap<MediaId, u64>,
    /// Did the composite currently being resolved put a decoded picture on at
    /// least one layer? Reset by `begin_frame`, so it describes this frame and
    /// not a frame from a minute ago.
    served_real: bool,
    /// Sources currently running forward under an `Exact` fetch.
    rolling: HashSet<MediaId>,
    /// Consecutive frame requests a source has answered with nothing.
    /// Cleared the moment it produces a picture, so a damaged region does
    /// not accumulate against a file that otherwise plays.
    failures: HashMap<MediaId, u32>,
    /// Sources written off as undecodable: asked for, repeatedly, and never
    /// answered. Nothing is requested for these again.
    undecodable: HashSet<MediaId>,
    /// How many times a pipeline was actually asked for a frame. Only a test
    /// reads it — it is the difference between "gave up" and "still trying
    /// but quietly", which no other observable distinguishes.
    attempts: u64,
    fetch: Fetch,
    timeout: Duration,
    /// Set for an export: a source is never written off and never stood in
    /// for by an older picture, because the caller decides when to give up.
    strict: bool,
    /// Indexed media the current composite asked for and got no picture of
    /// at the requested instant, with the source time asked for. Reset by
    /// `begin_frame`.
    missing: Vec<(MediaId, f64)>,
}

/// How far ahead of the last request an `Exact` fetch will roll the pipeline
/// forward rather than seek. An export steps a frame at a time, so almost
/// every request lands well inside this; a cut to a different part of the
/// source does not, and takes a real seek.
const ROLL_AHEAD_MS: u64 = 2_000;

impl MediaFrameSource {
    /// Index `project`'s decodable media and open nothing yet — sessions are
    /// created on first use and pruned when they go idle.
    pub fn new(project: &Project, fetch: Fetch) -> Self {
        Self {
            sources: Self::index(project),
            pool: SessionPool::for_editor(),
            software: SessionPool::new(
                EngineConfig::default().software_only().frames_only(),
                2,
                Duration::from_secs(30),
            ),
            demoted: HashSet::new(),
            converters: Vec::new(),
            slot: 0,
            last: HashMap::new(),
            served: HashMap::new(),
            served_real: false,
            rolling: HashSet::new(),
            failures: HashMap::new(),
            undecodable: HashSet::new(),
            attempts: 0,
            fetch,
            timeout: media::DEFAULT_FRAME_TIMEOUT,
            strict: false,
            missing: Vec::new(),
        }
    }

    /// Whether this source has given up on `media`.
    ///
    /// A file the decoder never answers for is not a transient miss: the
    /// media bin holds whatever the project says it holds, including a
    /// document persisted before the import guard existed, or one replicated
    /// from a peer — and neither is re-probed. Without a terminal state the
    /// pipeline is rebuilt for it on every frame, each attempt paying the
    /// full frame timeout, which is what made an editor holding one bad file
    /// stall on a fixed cadence forever.
    pub fn is_undecodable(&self, media: MediaId) -> bool {
        self.undecodable.contains(&media)
    }

    /// Whether `media` may still be served a decoded picture: it has bytes
    /// this source can decode, and it has not been written off.
    pub fn may_serve(&self, media: MediaId) -> bool {
        self.sources.contains_key(&media) && !self.undecodable.contains(&media)
    }

    /// How many times a pipeline has been asked for a frame. For tests: it is
    /// what tells "stopped asking" apart from "still asking and still
    /// failing", which look identical from the outside.
    #[cfg(test)]
    pub fn decode_attempts(&self) -> u64 {
        self.attempts
    }

    /// Which of `project`'s media this source can decode, and how fast each
    /// runs — the frame cache buckets by real frames, so the rate matters.
    fn index(project: &Project) -> HashMap<MediaId, (media::Source, f64, Option<u64>)> {
        let mut sources = HashMap::new();
        for item in &project.media {
            // A title's picture is generated, not decoded, so it never wants
            // a pipeline.
            if !item.kind.is_visual() || item.kind.is_generated() {
                continue;
            }
            let resolved = item.source();
            if !resolved.is_decodable() {
                // A projected reference whose bytes have not hydrated. Its
                // layers fall back to the representative fill.
                continue;
            }
            let fps = if item.fps > 1.0 { item.fps } else { 30.0 };
            // A clip trimmed to run past its source's end holds the last
            // frame there: that is the picture, so it is what is asked for.
            let last_frame_ms = (item.duration > 0.0)
                .then(|| ((item.duration - 1.0 / fps).max(0.0) * 1000.0) as u64);
            sources.insert(
                item.id,
                (
                    crate::media::engine_source(&resolved.abs_path),
                    fps,
                    last_frame_ms,
                ),
            );
        }
        sources
    }

    /// Re-index against `project` in place, keeping every open session.
    ///
    /// Rebuilding the whole source instead would tear down the pipelines and
    /// the YUV converters on every import — the two most expensive things it
    /// owns — so the editor holds one of these for its lifetime and calls
    /// this when the media bin changes. Cached pictures for media that has
    /// gone are dropped; everything still present keeps its session.
    pub fn set_project(&mut self, project: &Project) {
        self.sources = Self::index(project);
        self.last.retain(|id, _| self.sources.contains_key(id));
        self.served.retain(|id, _| self.sources.contains_key(id));
        self.rolling.retain(|id| self.sources.contains_key(id));
        self.demoted.retain(|id| self.sources.contains_key(id));
        self.failures.retain(|id, _| self.sources.contains_key(id));
        // A write-off is forgotten when the item leaves the bin, so
        // re-importing the file asks the decoder again rather than
        // inheriting a verdict about a project the user has since changed.
        self.undecodable.retain(|id| self.sources.contains_key(id));
    }

    /// A cheap signature of what [`set_project`](Self::set_project) would
    /// index, so a caller can skip the re-index when nothing has changed.
    pub fn project_signature(project: &Project) -> u64 {
        // FNV-1a over each decodable item's id and path. Order-independent
        // (the per-item hashes are summed) because the bin's order is not
        // part of what this source cares about.
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

    /// How long an `Exact` fetch waits for a picture before giving up.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// For an export: a source that has not answered yet is never written
    /// off and never shown as the last picture it produced. The caller reads
    /// [`missing_pictures`](Self::missing_pictures) after each composite and
    /// asks again, or gives up, itself.
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// The media the last composite asked for and got no picture of at the
    /// requested instant (with the source time), so its layers drew a
    /// stand-in. Empty when every decoded layer is the real frame.
    pub fn missing_pictures(&self) -> &[(MediaId, f64)] {
        &self.missing
    }

    /// Whether the last composite this source served actually resolved a
    /// decoded picture, rather than leaving every layer to its representative
    /// fill.
    ///
    /// This exists because "the preview has a real frame" must be answered by
    /// the renderer that *draws the canvas*. It used to be answered by the
    /// legacy `PreviewDecoder`, which the compositor replaced — so the
    /// screenshot path waited on a decoder that no longer fed the picture and
    /// captured the representative fill while believing it had the footage.
    pub fn served_real_picture(&self) -> bool {
        self.served_real
    }

    /// How many of the project's media this source can decode.
    pub fn decodable(&self) -> usize {
        self.sources.len()
    }

    /// What the pipeline plugged for `media`, for a diagnostic overlay.
    pub fn hw_report(&mut self, media: MediaId) -> Option<media::HwReport> {
        let (source, _, _) = self.sources.get(&media)?.clone();
        let session = self.pool_for(media).session(&source);
        let report = session.lock().ok()?.hw_report();
        Some(report)
    }

    fn pool_for(&mut self, media: MediaId) -> &mut SessionPool {
        if self.demoted.contains(&media) {
            &mut self.software
        } else {
            &mut self.pool
        }
    }

    /// Drop sessions nothing has touched lately. An open pipeline holds a
    /// decoder and a slice of the GPU; a closed clip should not keep one.
    pub fn prune(&mut self) {
        self.pool.prune();
        self.software.prune();
    }

    /// Paint one decoded picture into a texture the compositor can sample.
    fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        frame: &Frame,
    ) -> Option<SourceTexture> {
        let slot = self.slot;
        let painted = match &frame.payload {
            // The fast path on a hardware decoder: planes up, conversion in a
            // shader, nothing converted on the CPU.
            FramePayload::Planar(img) => {
                let planes = upload_planar(device, queue, img, frame.width, frame.height)?;
                while self.converters.len() <= slot {
                    self.converters
                        .push(YuvConverter::new(device, wgpu::TextureFormat::Rgba8Unorm));
                }
                // The loop above guarantees a converter at `slot`.
                let texture = self
                    .converters
                    .get_mut(slot)?
                    .convert(device, queue, &planes)
                    .clone();
                let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));
                Some(SourceTexture {
                    texture: Arc::new(texture),
                    view,
                    width: frame.width,
                    height: frame.height,
                })
            }
            // Software decode already landed in system memory.
            FramePayload::Rgba(bytes) => {
                upload_rgba(device, queue, frame.width, frame.height, bytes)
            }
            // GPU-resident and unimportable on this stack. Move the source to
            // a software session so its next frame arrives paintable, and
            // fall back for this one.
            FramePayload::Dmabuf(image) => {
                if self.demoted.insert(media) {
                    tracing::info!(
                        "video: {media} cannot be imported ({}), decoding it in software instead",
                        image.describe(),
                    );
                }
                None
            }
        }?;
        self.slot += 1;
        self.last.insert(
            media,
            LastGood {
                pts_ms: frame.pts_ms,
                texture: painted.clone(),
            },
        );
        Some(painted)
    }
}

impl FrameSource for MediaFrameSource {
    fn begin_frame(&mut self) {
        self.slot = 0;
        self.served_real = false;
        self.missing.clear();
    }

    fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        source_time: f64,
    ) -> Option<SourceTexture> {
        // Written off: never open a pipeline for it again. The compositor
        // falls back to the representative fill, exactly as it does for a
        // reference whose bytes have not hydrated.
        let (source, fps, last_frame_ms) = self.sources.get(&media).cloned()?;
        if self.undecodable.contains(&media) {
            self.missing.push((media, source_time));
            return None;
        }
        let target_ms = (source_time.max(0.0) * 1000.0) as u64;
        let target_ms = last_frame_ms.map_or(target_ms, |last| target_ms.min(last));
        self.attempts += 1;
        let fetch = self.fetch;
        let timeout = self.timeout;
        // Read where this source was last asked for *before* recording the
        // new request, or every step would look like a step of zero.
        let previously_served = self.served.insert(media, target_ms);

        let session = self.pool_for(media).session(&source);
        let frame = 'fetched: {
            let Ok(mut s) = session.lock() else {
                break 'fetched None;
            };
            s.set_fps(fps);
            match fetch {
                // An export walks the timeline forward, so a clip's source
                // time walks forward too and the next frame is simply the
                // next one out of the decoder. Seeking for each of them
                // would pay a full accurate-seek per frame — correct, and
                // roughly a second each. Roll the pipeline instead, and
                // keep the seek for a genuine jump.
                Fetch::Exact => {
                    let step_ahead = matches!(
                        previously_served,
                        Some(prev) if target_ms >= prev && target_ms - prev <= ROLL_AHEAD_MS,
                    );
                    let frame = if step_ahead {
                        if self.rolling.insert(media) {
                            s.engine().play();
                        }
                        let deadline = ui::time::now() + timeout;
                        loop {
                            if let Some(f) = s.cached_frame_at(target_ms) {
                                break Some(f);
                            }
                            if ui::time::now() >= deadline {
                                break None;
                            }
                            s.poll_into_cache();
                        }
                    } else {
                        None
                    };
                    if frame.is_some() {
                        frame
                    } else {
                        // Either a jump, or the roll did not reach the
                        // target in time — fall back to the exact seek,
                        // which always lands on the right picture.
                        self.rolling.remove(&media);
                        s.frame_at(target_ms, timeout)
                    }
                }
                Fetch::Responsive => {
                    if let Some(f) = s.cached_frame_at(target_ms) {
                        Some(f)
                    } else {
                        // Ask the pipeline to move, pump it once, and take
                        // whatever has landed. Never blocks.
                        s.engine().seek_ms(target_ms);
                        s.poll_into_cache();
                        s.cached_frame_at(target_ms)
                    }
                }
            }
        };

        if let Some(f) = frame {
            // It answered, so whatever it missed before was transient.
            self.failures.remove(&media);
            // Already painted this exact picture for this source? Reuse
            // the texture rather than uploading it again.
            if let Some(last) = self.last.get(&media)
                && last.pts_ms == f.pts_ms
            {
                let texture = last.texture.clone();
                self.slot += 1;
                self.served_real = true;
                return Some(texture);
            }
            let painted = self.paint(device, queue, media, &f);
            self.served_real |= painted.is_some();
            if painted.is_none() {
                self.missing.push((media, source_time));
            }
            painted
        } else {
            self.missing.push((media, source_time));
            if self.strict {
                return None;
            }
            // Nothing decoded yet. Hold the last picture this source produced
            // so a scrub does not flash the representative fill between
            // seeks; with nothing at all, the compositor falls back.
            // Count it, and stop asking once a source has answered
            // nothing this many times running. One miss is an ordinary
            // seek landing near a damaged GOP; a file that answers
            // nowhere is not media this editor can play, and retrying it
            // costs a full frame timeout every frame forever.
            let n = self.failures.entry(media).or_insert(0);
            *n += 1;
            if *n >= crate::thumbnail::FAILURES_BEFORE_GIVING_UP {
                self.undecodable.insert(media);
                self.rolling.remove(&media);
            }
            let texture = self.last.get(&media).map(|l| l.texture.clone());
            if texture.is_some() {
                self.slot += 1;
            }
            texture
        }
    }
}
