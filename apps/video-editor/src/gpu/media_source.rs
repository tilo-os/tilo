//! Decoded pictures for the compositor, from the media engine: the [`FrameSource`]
//! the preview canvas and the export both pull layers through.
//!
//! A hardware decoder emits NV12, so the planar payload goes to the GPU as planes
//! and the YUV arithmetic happens in a shader; no colour conversion runs on the
//! CPU. A software decode arrives as RGBA. A `Dmabuf` cannot be imported on this
//! stack (wgpu-hal does not enable `VK_EXT_image_drm_format_modifier`, and VA-API
//! hands out tiled buffers), so a source yielding one moves to a software session
//! rather than dropping the frame.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use media::{EngineConfig, Frame, FramePayload, SessionPool};
use ui::gpu_video::{YuvConverter, upload_planar};
use vello::wgpu;

use crate::model::{MediaId, Project};

use super::{FrameSource, SourceTexture, compositor::upload_rgba};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch {
    /// Block until the frame at the requested instant is decoded: an export wants
    /// every frame right and has no interactive deadline.
    Exact,
    /// Never block: serve the cached frame, else move the pipeline and paint the
    /// last picture until it arrives. A scrub must not stall the UI thread.
    Responsive,
}

#[derive(Clone)]
struct LastGood {
    pts_ms: u64,
    texture: SourceTexture,
    frame: Arc<Frame>,
}

pub struct MediaFrameSource {
    pool: SessionPool,
    software: SessionPool,
    demoted: HashSet<MediaId>,
    /// Decodable media: where the bytes are, how fast the source runs (the frame
    /// cache buckets by real frames), and the instant of its last frame.
    sources: HashMap<MediaId, (media::Source, f64, Option<u64>)>,
    /// One per layer slot: each owns an output texture it reuses, so two layers in
    /// one composite must not share one — the second would overwrite the first.
    converters: Vec<YuvConverter>,
    slot: usize,
    last: HashMap<MediaId, LastGood>,
    served: HashMap<MediaId, u64>,
    accepted_seeks: HashMap<MediaId, u64>,
    /// Reset by `begin_frame`, so it describes this frame.
    served_real: bool,
    rolling: HashSet<MediaId>,
    /// Cleared the moment a source produces a picture, so a damaged region does
    /// not accumulate against a file that otherwise plays.
    failures: HashMap<MediaId, u32>,
    /// A missed responsive poll is not a failed seek: allow a whole decode window
    /// before counting a miss, whatever the repaint rate.
    failure_started: HashMap<MediaId, ui::time::Instant>,
    undecodable: HashSet<MediaId>,
    /// Only a test reads it: it is the difference between "gave up" and "still
    /// trying but quietly".
    attempts: u64,
    fetch: Fetch,
    timeout: Duration,
    /// Set for an export: nothing is written off and nothing is stood in for by an
    /// older picture, because the caller decides when to give up.
    strict: bool,
    missing: Vec<(MediaId, f64)>,
}

/// An export steps a frame at a time and lands inside this; a jump to another part
/// of the source takes a real seek.
const ROLL_AHEAD_MS: u64 = 2_000;

/// Rounded up rather than down: at 30 fps the 62nd picture begins at 2066.67 ms,
/// and a seek to 2066 is answered with the picture before it, stamped 2066 because
/// the decoder clips it to where the seek landed — so a frame is repeated and the
/// next dropped. Rounding up stays inside the picture that starts at `seconds`.
fn source_ms(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1000.0).ceil() as u64
}

impl MediaFrameSource {
    /// Index the decodable media and open nothing yet: sessions are created on first
    /// use and pruned when idle.
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
            accepted_seeks: HashMap::new(),
            served_real: false,
            rolling: HashSet::new(),
            failures: HashMap::new(),
            failure_started: HashMap::new(),
            undecodable: HashSet::new(),
            attempts: 0,
            fetch,
            timeout: media::DEFAULT_FRAME_TIMEOUT,
            strict: false,
            missing: Vec::new(),
        }
    }

    /// A file the decoder never answers for is not a transient miss: without a
    /// terminal state the pipeline is rebuilt for it every frame, each attempt paying
    /// the full timeout.
    pub fn is_undecodable(&self, media: MediaId) -> bool {
        self.undecodable.contains(&media)
    }

    pub fn has_pending_frames(&self) -> bool {
        self.failure_started.keys().any(|id| self.may_serve(*id))
    }

    pub fn may_serve(&self, media: MediaId) -> bool {
        self.sources.contains_key(&media) && !self.undecodable.contains(&media)
    }

    #[cfg(test)]
    pub fn decode_attempts(&self) -> u64 {
        self.attempts
    }

    fn index(project: &Project) -> HashMap<MediaId, (media::Source, f64, Option<u64>)> {
        let mut sources = HashMap::new();
        for item in &project.media {
            // A title's picture is generated, so it never wants a pipeline.
            if !item.kind.is_visual() || item.kind.is_generated() {
                continue;
            }
            let resolved = item.source();
            if !resolved.is_decodable() {
                continue;
            }
            let fps = if item.fps > 1.0 { item.fps } else { 30.0 };
            // A clip running past its source's end holds the last frame, and a still
            // has one picture however long its clip runs.
            let last_frame_ms = if item.kind == crate::model::MediaKind::Image {
                Some(0)
            } else {
                (item.duration > 0.0).then(|| source_ms(item.duration - 1.0 / fps))
            };
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

    /// Rebuilding the source instead would tear down the pipelines and the YUV
    /// converters on every import, so the editor holds one for its lifetime and
    /// calls this when the media bin changes.
    pub fn set_project(&mut self, project: &Project) {
        self.sources = Self::index(project);
        self.last.retain(|id, _| self.sources.contains_key(id));
        self.served.retain(|id, _| self.sources.contains_key(id));
        self.accepted_seeks
            .retain(|id, _| self.sources.contains_key(id));
        self.rolling.retain(|id| self.sources.contains_key(id));
        self.demoted.retain(|id| self.sources.contains_key(id));
        self.failures.retain(|id, _| self.sources.contains_key(id));
        self.failure_started
            .retain(|id, _| self.sources.contains_key(id));
        // A write-off is forgotten when the item leaves the bin, so re-importing
        // asks the decoder again.
        self.undecodable.retain(|id| self.sources.contains_key(id));
    }

    pub fn project_signature(project: &Project) -> u64 {
        super::media_signature(project)
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The caller reads [`missing_pictures`](Self::missing_pictures) after each
    /// composite and asks again, or gives up, itself.
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    pub fn missing_pictures(&self) -> &[(MediaId, f64)] {
        &self.missing
    }

    pub fn served_real_picture(&self) -> bool {
        self.served_real
    }

    pub fn has_picture(&self, media: MediaId) -> bool {
        self.last.contains_key(&media)
    }

    pub fn pixel(&self, media: MediaId, u: f64, v: f64) -> Option<[f64; 3]> {
        sample(&self.last.get(&media)?.frame, u, v)
    }

    pub fn decodable(&self) -> usize {
        self.sources.len()
    }

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

    /// An open pipeline holds a decoder and a slice of the GPU; a closed clip should
    /// not keep one.
    pub fn prune(&mut self) {
        self.pool.prune();
        self.software.prune();
    }

    fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        frame: &Arc<Frame>,
    ) -> Option<SourceTexture> {
        let slot = self.slot;
        let painted = match &frame.payload {
            FramePayload::Planar(img) => {
                let planes = upload_planar(device, queue, img, frame.width, frame.height)?;
                while self.converters.len() <= slot {
                    self.converters
                        .push(YuvConverter::new(device, wgpu::TextureFormat::Rgba8Unorm));
                }
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
            FramePayload::Rgba(bytes) => {
                upload_rgba(device, queue, frame.width, frame.height, bytes)
            }
            // GPU-resident and unimportable here: move the source to a software
            // session and stand in for this frame.
            FramePayload::Dmabuf(image) => {
                if self.demoted.insert(media) {
                    self.accepted_seeks.remove(&media);
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
                frame: Arc::clone(frame),
            },
        );
        Some(painted)
    }
}

fn sample(frame: &Frame, across: f64, down: f64) -> Option<[f64; 3]> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let rgba = frame.to_rgba()?;
    let column = ((across.clamp(0.0, 1.0) * f64::from(frame.width)) as u32).min(frame.width - 1);
    let row = ((down.clamp(0.0, 1.0) * f64::from(frame.height)) as u32).min(frame.height - 1);
    let at = ((row * frame.width + column) * 4) as usize;
    let &[red, green, blue] = rgba.get(at..at + 3)? else {
        return None;
    };
    Some([red, green, blue].map(|channel| f64::from(channel) / 255.0))
}

impl MediaFrameSource {
    /// One seek per target: reissuing it on every repaint flushes an in-flight
    /// decode before its preroll lands, leaving a paused preview blank.
    fn responsive_frame(
        &mut self,
        s: &mut media::session::EngineSession,
        media: MediaId,
        target_ms: u64,
    ) -> Option<Arc<Frame>> {
        if let Some(f) = s.cached_frame_at(target_ms) {
            return Some(f);
        }
        if self.accepted_seeks.get(&media) != Some(&target_ms)
            && s.engine().seek_with(target_ms, media::SeekMode::Accurate)
        {
            self.accepted_seeks.insert(media, target_ms);
        }
        s.poll_into_cache();
        s.cached_frame_at(target_ms).or_else(|| {
            // An accurate seek returns the picture covering the target, whose own PTS
            // may fall in another cache bucket, so use the accepted generation rather
            // than waiting for an exact PTS.
            (self.accepted_seeks.get(&media) == Some(&target_ms) && s.engine().frame_is_current())
                .then(|| s.engine().latest().cloned().map(Arc::new))
                .flatten()
        })
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
        let (source, fps, last_frame_ms) = self.sources.get(&media).cloned()?;
        if self.undecodable.contains(&media) {
            self.missing.push((media, source_time));
            return None;
        }
        let target_ms = source_ms(source_time);
        let target_ms = last_frame_ms.map_or(target_ms, |last| target_ms.min(last));
        self.attempts += 1;
        let fetch = self.fetch;
        let timeout = self.timeout;
        let previously_served = self.served.insert(media, target_ms);

        let session = self.pool_for(media).session(&source);
        let frame = 'fetched: {
            let Ok(mut s) = session.lock() else {
                break 'fetched None;
            };
            s.set_fps(fps);
            match fetch {
                // Seeking per frame would pay a full accurate seek each time, so roll
                // the pipeline forward and keep the seek for a genuine jump.
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
                        // A jump, or the roll did not reach the target: fall back to
                        // the exact seek. Stop the roll first, since a running pipeline
                        // keeps delivering frames for where it was.
                        if self.rolling.remove(&media) {
                            s.engine().pause();
                        }
                        s.frame_at(target_ms, timeout)
                    }
                }
                Fetch::Responsive => self.responsive_frame(&mut s, media, target_ms),
            }
        };

        if let Some(f) = frame {
            self.failures.remove(&media);
            self.failure_started.remove(&media);
            if let Some(last) = self.last.get(&media)
                && last.pts_ms == f.pts_ms
            {
                let texture = last.texture.clone();
                self.slot += 1;
                self.served_real = true;
                return Some(texture);
            }
            let painted = self.paint(device, queue, media, &f);
            if painted.is_none() {
                self.failure_started
                    .entry(media)
                    .or_insert_with(ui::time::now);
            }
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
            // Hold the last picture so a scrub does not flash the representative
            // fill between seeks. Stop asking once a source has answered nothing this
            // many times running: a file that answers nowhere is not media this editor
            // can play, and retrying costs a full timeout every frame forever.
            let now = ui::time::now();
            let since = self.failure_started.entry(media).or_insert(now);
            if fetch == Fetch::Exact || now.duration_since(*since) >= timeout {
                *since = now;
                let n = self.failures.entry(media).or_insert(0);
                *n += 1;
                if *n >= crate::thumbnail::FAILURES_BEFORE_GIVING_UP {
                    self.undecodable.insert(media);
                    self.rolling.remove(&media);
                }
            }
            let texture = self.last.get(&media).map(|l| l.texture.clone());
            if texture.is_some() {
                self.slot += 1;
            }
            texture
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaFrameSource, sample, source_ms};
    use crate::model::{MediaItem, MediaKind, Project};

    #[test]
    fn a_still_is_asked_for_its_one_picture_wherever_its_clip_is() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let mut project = Project::new("Stills");
        let mut item = |name: &str, kind: MediaKind| {
            let path = dir.path().join(name);
            std::fs::write(&path, b"bytes").expect("a local file");
            let item = MediaItem {
                id: uuid::Uuid::new_v4(),
                name: name.into(),
                kind,
                duration: 5.0,
                fps: 30.0,
                path: path.to_string_lossy().into_owned(),
                ..Default::default()
            };
            let id = item.id;
            project.media.push(item);
            id
        };
        let still = item("Harbour.png", MediaKind::Image);
        let footage = item("Harbour.mp4", MediaKind::Video);
        let index = MediaFrameSource::index(&project);
        assert_eq!(
            index[&still].2,
            Some(0),
            "a still's picture is at its start"
        );
        assert!(
            index[&footage].2.is_some_and(|last| last > 4000),
            "footage holds its last frame: {:?}",
            index[&footage].2
        );
    }

    #[test]
    fn a_frame_is_asked_for_inside_itself_not_inside_the_one_before() {
        for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
            for n in 0..600u32 {
                let start = f64::from(n) / fps;
                let ms = source_ms(start) as f64 / 1000.0;
                assert!(
                    ms >= start - 1e-9 && ms < start + 1.0 / fps,
                    "frame {n} at {fps} fps starts at {start}s but is asked for at {ms}s"
                );
            }
        }
    }

    #[test]
    fn a_sample_reads_the_pixel_under_the_coordinates() {
        let mut rgba = Vec::new();
        for _ in 0..2 {
            rgba.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255]);
            rgba.extend_from_slice(&[0, 0, 255, 255, 0, 0, 255, 255]);
        }
        let frame = media::Frame {
            width: 4,
            height: 2,
            pts_ms: 0,
            payload: media::FramePayload::Rgba(std::sync::Arc::new(rgba)),
        };
        assert_eq!(sample(&frame, 0.1, 0.5), Some([1.0, 0.0, 0.0]));
        assert_eq!(sample(&frame, 1.0, 1.0), Some([0.0, 0.0, 1.0]));
    }
}
